#[cfg(target_os = "macos")]
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// PID of the live QEMU child process, or -1 when none is running.
/// Written by `QemuInstance::spawn`, cleared by `stop()` / `Drop`.
pub static QEMU_CHILD_PID: AtomicI32 = AtomicI32::new(-1);

/// How long EP122's sub-CPU takes to unmount USB after a power-off stimulus.
/// Mirrors the countdown on real hardware.
const EP122_CLEANUP_WAIT: Duration = Duration::from_secs(8);
/// How long systemd needs to walk the unit graph for ACPI shutdown.
const ACPI_SHUTDOWN_WAIT: Duration = Duration::from_secs(20);
/// Window between sending QMP `quit` and SIGKILL-ing the QEMU child.
const QMP_QUIT_WAIT: Duration = Duration::from_secs(5);
/// External (signal-handler-safe) SIGTERM grace before SIGKILL.
const SIGTERM_GRACE: Duration = Duration::from_secs(3);
/// Poll cadence for `wait_or_kill` / `kill_qemu_child` while waiting for the
/// QEMU child to exit. Short enough to feel responsive on a clean shutdown,
/// long enough not to busy-loop on a stuck guest.
const PROCESS_WAIT_POLL: Duration = Duration::from_millis(100);
/// Same idea, signal-safe variant used in `kill_qemu_child` (no atomics involved).
const SIGTERM_POLL: Duration = Duration::from_millis(50);
/// ivshmem jog-LCD buffer size. Layout fits 320×240 XRGB plus header in &lt;1 MiB.
const JOG_SHM_BYTES: u64 = 1 << 20;
/// Prefill size for `main.shm`. Large enough to cover any future stride choice
/// without forcing QEMU to grow the file (the reader holds a fixed mmap and
/// cannot follow `ftruncate`). 1280×720 RGBA8888 + header ≈ 3.6 MiB - 8 MiB
/// leaves a generous margin.
const MAIN_SHM_PREFILL: u64 = 8 * 1024 * 1024;

/// Graceful child shutdown for use from `on_exit` (main thread, blocking OK).
/// SIGTERM → up to [`SIGTERM_GRACE`] → SIGKILL.
pub fn kill_qemu_child() {
    let pid = QEMU_CHILD_PID.load(Ordering::Relaxed);
    if pid <= 0 {
        return;
    }
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    let deadline = Instant::now() + SIGTERM_GRACE;
    while Instant::now() < deadline {
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            break; // process gone
        }
        std::thread::sleep(SIGTERM_POLL);
    }
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    QEMU_CHILD_PID.store(-1, Ordering::Relaxed);
}

/// Signal-safe child kill for use inside signal handlers (no sleep, no alloc).
pub fn kill_qemu_child_now() {
    let pid = QEMU_CHILD_PID.load(Ordering::Relaxed);
    if pid > 0 {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
}

use cdj3k_emu_platform::menu_state;

#[cfg(target_os = "linux")]
use crate::config::LinuxQemuConfig;
#[cfg(target_os = "macos")]
use crate::config::QemuConfig;
use crate::qmp::{QmpClient, QmpError};

#[derive(Debug)]
pub enum InstanceError {
    AlreadyRunning,
    DylibUnavailable,
    QmpConnect(QmpError),
    SockDir(std::io::Error),
    EmmcLocked(PathBuf),
    #[cfg(target_os = "linux")]
    InstanceLocked(PathBuf),
}

impl From<QmpError> for InstanceError {
    fn from(e: QmpError) -> Self {
        InstanceError::QmpConnect(e)
    }
}

struct Inner {
    /// Monitor thread: waits on the QEMU child process.
    thread: Option<JoinHandle<i32>>,
    qmp: QmpClient,
    /// PID of the QEMU subprocess, used for SIGKILL.
    pid: u32,
    /// Linux run-directory ownership. macOS retains the upstream eMMC-only
    /// locking contract and does not create an instance-directory lock.
    #[cfg(target_os = "linux")]
    _instance_lock: Option<std::fs::File>,
    /// Held for the lifetime of the instance - blocks any other cdj3k-emu process
    /// from booting against the same eMMC qcow2 (qcow2 is not concurrent-safe).
    _emmc_lock: Option<std::fs::File>,
}

/// A running QEMU instance.  Call `stop()` or let `Drop` send a quit + SIGKILL.
pub struct QemuInstance {
    #[cfg(target_os = "macos")]
    config: QemuConfig,
    /// Runtime directory owned by this launch. Linux may use an explicit
    /// per-instance directory rather than the macOS platform default.
    #[cfg(target_os = "linux")]
    runtime_dir: PathBuf,
    #[cfg(target_os = "linux")]
    serial_log_path: PathBuf,
    #[cfg(target_os = "linux")]
    qmp_socket_path: Option<PathBuf>,
    inner: Inner,
    running: Arc<AtomicBool>,
}

impl QemuInstance {
    /// Spawn QEMU as a child subprocess (re-exec self with --qemu-worker).
    /// Kills any stale QEMU from a previous .app run before spawning.
    #[cfg(target_os = "macos")]
    pub fn spawn(config: QemuConfig) -> Result<Self, InstanceError> {
        kill_stale(config.qmp_port, &config.sock_dir());

        std::fs::create_dir_all(config.sock_dir()).map_err(InstanceError::SockDir)?;

        // Exclusive non-blocking flock on the eMMC qcow2 - prevents two cdj3k-emu
        // instances from corrupting the same image. The lock is released when
        // the file handle held in Inner is dropped.
        let emmc_lock = if let Some(emmc) = &config.emmc_img {
            let f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(emmc)
                .map_err(InstanceError::SockDir)?;
            let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                return Err(InstanceError::EmmcLocked(emmc.clone()));
            }
            Some(f)
        } else {
            None
        };

        if config.shm {
            // Guest RAM file matches QemuConfig::MEM_BYTES; sparse so host disk usage is zero.
            prefill_sparse(&config.shm_path(), crate::config::QemuConfig::MEM_BYTES)
                .map_err(InstanceError::SockDir)?;
        }

        // Prefill main.shm to the max framebuffer size BEFORE QEMU starts.
        // The reader (cdj3k-emu-streams) mmaps the file at open time and never
        // resizes its mapping; if QEMU later grew the file via ftruncate, the
        // reader would see frames too large for its mmap and silently drop
        // them ("Display output is not active" forever). Prefilling avoids the
        // grow path entirely - QEMU's shm_remap fstats, sees the file is
        // already big enough, and skips ftruncate. Reader sees a zero header
        // (no magic) until QEMU writes it, so it just waits.
        prefill_sparse(&config.sock_dir().join("main.shm"), MAIN_SHM_PREFILL)
            .map_err(InstanceError::SockDir)?;

        // ivshmem jog frame buffer. Always recreate fresh so a stale
        // 'JOG1' magic from a previous run doesn't fool the host into reading
        // the prior session's pixels before the guest publishes the first frame.
        prefill_sparse(&config.jog_shm_path(), JOG_SHM_BYTES).map_err(InstanceError::SockDir)?;

        // 1-sector placeholder so the USB virtio-blk slot has a valid backing file at boot.
        {
            let ph = config.usb_placeholder_path();
            if !ph.exists() {
                prefill_sparse(&ph, 512).map_err(InstanceError::SockDir)?;
            }
        }

        let qemu_argv = config.build_argv();
        let self_exe = std::env::current_exe().map_err(InstanceError::SockDir)?;

        eprintln!(
            "cdj3k-emu: spawning QEMU subprocess: {} --qemu-worker {}",
            self_exe.display(),
            qemu_argv.join(" ")
        );

        let tap_fd = config.net_tap_fd;
        let mut cmd = std::process::Command::new(&self_exe);
        cmd.arg("--qemu-worker").args(&qemu_argv);
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(move || {
                // Re-clear FD_CLOEXEC after fork so the tap fd reaches cdj3k_emu_qemu_run.
                if let Some(fd) = tap_fd {
                    libc::fcntl(fd, libc::F_SETFD, 0);
                }
                // Pin QEMU's main thread to USER_INTERACTIVE QoS so macOS keeps
                // it on P-cores. Process-default QoS is inherited by QEMU
                // threads spawned later (audio callback, vCPU threads), which
                // reduces gap_max in the virtio_snd TX-return path.
                extern "C" {
                    fn pthread_set_qos_class_self_np(
                        qos_class: u32,
                        relative_priority: i32,
                    ) -> libc::c_int;
                }
                const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
                pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(InstanceError::SockDir)?;

        let pid = child.id();
        QEMU_CHILD_PID.store(pid as i32, Ordering::Relaxed);
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        let thread = std::thread::Builder::new()
            .name(format!("qemu-monitor-{}", config.instance_id))
            .spawn(move || {
                let code = child
                    .wait_with_output()
                    .map(|o| o.status.code().unwrap_or(-1))
                    .unwrap_or(-1);
                eprintln!("cdj3k-emu: QEMU subprocess exited with code {code}");
                running_clone.store(false, Ordering::Release);
                code
            })
            .expect("failed to spawn QEMU monitor thread");

        let qmp = QmpClient::connect_with_retry(config.qmp_port, Duration::from_secs(15))?;

        Ok(Self {
            inner: Inner {
                thread: Some(thread),
                qmp,
                pid,
                _emmc_lock: emmc_lock,
            },
            config,
            running,
        })
    }

    /// Spawn the system QEMU binary used by the Linux/WSL application.
    /// Unlike the macOS path this does not re-exec the application or use the
    /// platform FFI; the child process is directly owned by this instance.
    #[cfg(target_os = "linux")]
    pub fn spawn_linux(config: LinuxQemuConfig) -> Result<Self, InstanceError> {
        let mut config = config;
        let launch_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        config.qmp_socket = Some(config.guest.run_dir.join(format!("qmp-{launch_id}.sock")));
        let g = &config.guest;
        std::fs::create_dir_all(&g.run_dir).map_err(InstanceError::SockDir)?;
        let instance_lock = acquire_instance_lock(&g.run_dir)?;
        remove_stale_qmp_sockets(&g.run_dir);
        prefill_sparse(&g.main_shm_path(), MAIN_SHM_PREFILL).map_err(InstanceError::SockDir)?;
        prefill_sparse(&g.jog_shm_path(), JOG_SHM_BYTES).map_err(InstanceError::SockDir)?;
        if config.virtual_media {
            prefill_sparse(&g.run_dir.join("usb.empty.medium"), 512)
                .map_err(InstanceError::SockDir)?;
        }

        let emmc_lock = if let Some(emmc) = &g.emmc_img {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(emmc)
                .map_err(InstanceError::SockDir)?;
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                return Err(InstanceError::EmmcLocked(emmc.clone()));
            }
            Some(file)
        } else {
            None
        };

        let serial_log_path = config.serial_log.clone().unwrap_or_else(|| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            g.run_dir.join(format!("serial-{nanos}.log"))
        });
        config.serial_log = Some(serial_log_path.clone());

        let mut command = std::process::Command::new(&config.qemu);
        command.args(config.build_argv()).current_dir(&g.run_dir);
        unsafe {
            use std::os::unix::process::CommandExt;
            let parent_pid = libc::getpid();
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent_pid {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "parent exited before QEMU exec",
                    ));
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(InstanceError::SockDir)?;
        let pid = child.id();
        QEMU_CHILD_PID.store(pid as i32, Ordering::Relaxed);
        let running = Arc::new(AtomicBool::new(true));
        let qmp_path = config.qmp_socket.as_ref().expect("Linux QMP socket set");
        let qmp_deadline = Instant::now() + Duration::from_secs(15);
        let qmp = loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    QEMU_CHILD_PID.store(-1, Ordering::Relaxed);
                    let _ = std::fs::remove_file(qmp_path);
                    return Err(InstanceError::QmpConnect(QmpError::QemuError(format!(
                        "QEMU exited before QMP negotiation: {status}"
                    ))));
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    QEMU_CHILD_PID.store(-1, Ordering::Relaxed);
                    let _ = std::fs::remove_file(qmp_path);
                    return Err(InstanceError::SockDir(error));
                }
            }
            match QmpClient::connect_unix(qmp_path) {
                Ok(qmp) => break qmp,
                Err(error) if Instant::now() < qmp_deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                    let _ = error;
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    QEMU_CHILD_PID.store(-1, Ordering::Relaxed);
                    let _ = std::fs::remove_file(qmp_path);
                    return Err(InstanceError::QmpConnect(error));
                }
            }
        };

        let running_clone = Arc::clone(&running);
        let instance_id = g.instance_id;
        let thread = std::thread::Builder::new()
            .name(format!("qemu-linux-monitor-{instance_id}"))
            .spawn(move || {
                let code = child
                    .wait()
                    .map(|status| status.code().unwrap_or(-1))
                    .unwrap_or(-1);
                running_clone.store(false, Ordering::Release);
                code
            })
            .map_err(InstanceError::SockDir)?;

        Ok(Self {
            runtime_dir: g.run_dir.clone(),
            serial_log_path,
            qmp_socket_path: Some(qmp_path.clone()),
            inner: Inner {
                thread: Some(thread),
                qmp,
                pid,
                _instance_lock: Some(instance_lock),
                _emmc_lock: emmc_lock,
            },
            running,
        })
    }

    pub fn sock_dir(&self) -> PathBuf {
        #[cfg(target_os = "macos")]
        {
            self.config.sock_dir()
        }
        #[cfg(target_os = "linux")]
        {
            self.runtime_dir.clone()
        }
    }

    #[cfg(target_os = "linux")]
    pub fn serial_log_path(&self) -> &Path {
        &self.serial_log_path
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Graceful stop: SPI power-off stimuli → 8 s for EP122 USB cleanup →
    /// ACPI system_powerdown → wait 20 s for guest halt → QMP quit + SIGKILL fallback.
    pub fn stop(&mut self) -> i32 {
        self.shutdown_sequence();
        let code = self
            .inner
            .thread
            .take()
            .and_then(|t| t.join().ok())
            .unwrap_or(-1);
        QEMU_CHILD_PID.store(-1, Ordering::Relaxed);
        #[cfg(target_os = "linux")]
        if let Some(path) = &self.qmp_socket_path {
            let _ = std::fs::remove_file(path);
        }
        code
    }

    /// Drive the guest through EP122 cleanup → ACPI powerdown → QMP quit →
    /// SIGKILL, but leave thread joining + global PID reset to the caller.
    /// Shared by `stop()` and `Drop`.
    fn shutdown_sequence(&mut self) {
        if self.inner.thread.is_none() {
            return;
        }
        // Linux's directly-owned child may already have been reaped by its
        // monitor thread. The upstream macOS lifecycle is deliberately
        // guarded only by monitor-thread presence.
        #[cfg(target_os = "linux")]
        if !self.running.load(Ordering::Acquire) {
            return;
        }
        menu_state::lock().power_off_stimuli_requested = true;
        // Give EP122 ~8 s to unmount USB (mirrors the real sub-CPU countdown).
        wait_or_kill(&self.running, self.inner.pid, EP122_CLEANUP_WAIT);
        if self.running.load(Ordering::Acquire) {
            // EP122 cleanup done; trigger clean Linux shutdown via ACPI.
            let _ = self.inner.qmp.system_powerdown();
            wait_or_kill(&self.running, self.inner.pid, ACPI_SHUTDOWN_WAIT);
        }
        if self.running.load(Ordering::Acquire) {
            let _ = self.inner.qmp.quit();
            wait_or_kill(&self.running, self.inner.pid, QMP_QUIT_WAIT);
        }
    }

    /// Stop and restart with a new config.
    #[cfg(target_os = "macos")]
    pub fn restart(&mut self, new_config: QemuConfig) -> Result<(), InstanceError> {
        self.stop();
        // Release the flock before spawn tries to re-acquire it on a new fd.
        self.inner._emmc_lock = None;
        let new = Self::spawn(new_config)?;
        let new = std::mem::ManuallyDrop::new(new);
        // SAFETY: `new` is wrapped in `ManuallyDrop`, so its destructor will
        // not run when `new` goes out of scope at the end of this function.
        // The three `ptr::read`s bitwise-move each field into `self`,
        // overwriting `self`'s old fields whose destructors already ran via
        // `self.stop()` above plus the `_emmc_lock = None` drop on line 265
        // (i.e. self's own resources are already released).  After the
        // moves, the source struct is logically uninitialised and
        // `ManuallyDrop` prevents a double-drop on the moved-from fields.
        unsafe {
            self.inner = std::ptr::read(&new.inner);
            self.config = std::ptr::read(&new.config);
            self.running = std::ptr::read(&new.running);
        }
        Ok(())
    }

    /// Direct QMP access for device_add / device_del.
    pub fn qmp(&mut self) -> &mut QmpClient {
        &mut self.inner.qmp
    }
}

impl Drop for QemuInstance {
    fn drop(&mut self) {
        self.shutdown_sequence();
        self.inner.thread.take().and_then(|t| t.join().ok());
        QEMU_CHILD_PID.store(-1, Ordering::Relaxed);
        #[cfg(target_os = "linux")]
        {
            if let Some(path) = &self.qmp_socket_path {
                let _ = std::fs::remove_file(path);
            }
            cleanup_linux_qemu_files_for_restart(&self.runtime_dir);
        }
        #[cfg(target_os = "macos")]
        cleanup_qemu_files_for_restart(&self.config.sock_dir());
    }
}

/// Poll until the process exits, then SIGKILL if it didn't within `timeout`.
fn wait_or_kill(running: &Arc<AtomicBool>, pid: u32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while running.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(PROCESS_WAIT_POLL);
    }
    if running.load(Ordering::Acquire) {
        eprintln!(
            "cdj3k-emu: QEMU did not exit within {}s - sending SIGKILL",
            timeout.as_secs()
        );
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
}

/// Create (or truncate) `path` to be a sparse file of exactly `len` bytes.
/// Used to prepare host-side mmaps so QEMU can map them at boot without
/// `ftruncate` growing the file out from under any active reader.
fn prefill_sparse(path: &Path, len: u64) -> std::io::Result<()> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    f.set_len(len)
}

/// Acquire the per-instance lock before touching any launch-owned endpoint.
/// The lock file itself is persistent and harmless; the advisory flock is
/// released automatically when the owning QemuInstance is dropped.
#[cfg(target_os = "linux")]
fn acquire_instance_lock(run_dir: &Path) -> Result<std::fs::File, InstanceError> {
    let path = run_dir.join(".instance.lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&path)
        .map_err(InstanceError::SockDir)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err(InstanceError::InstanceLocked(path));
    }
    Ok(file)
}

/// Remove only stale QMP sockets after the instance lock is held. No other
/// launcher for this instance can be using these files at this point.
#[cfg(target_os = "linux")]
fn remove_stale_qmp_sockets(run_dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(run_dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("qmp-") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Kill any stale QEMU from a previous .app run and remove its socket files.
#[cfg(target_os = "macos")]
fn kill_stale(qmp_port: u16, sock_dir: &Path) {
    let addr: std::net::SocketAddr = format!("127.0.0.1:{}", qmp_port).parse().unwrap();

    if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
        eprintln!("cdj3k-emu: stale QEMU detected on port {qmp_port}, sending quit");
        if let Ok(mut qmp) = QmpClient::connect(qmp_port) {
            let _ = qmp.quit();
        }
        // Wait up to 5 s for the QMP port to close.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_err() {
                break;
            }
        }
        // If still up, there's nothing more we can do without the PID.
    }

    // Use the restart-safe variant: SocketVmnet may already be live (set up
    // before this spawn during a network change), and wiping its socket here
    // would crash the daemon mid-restart. The vmnet sock is owned by
    // SocketVmnet's own lifetime, never by the QEMU spawn cycle.
    cleanup_qemu_files_for_restart(sock_dir);
}

/// Path registered by the app for shutdown-time cleanup. Set once at startup;
/// `cleanup_runtime_files` reads it from any exit path (eframe on_exit,
/// signal handler, atexit) without needing to thread state through the UI.
pub static SHUTDOWN_SOCK_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Wipe the sock dir registered via `SHUTDOWN_SOCK_DIR`. No-op if unset.
/// Safe to call repeatedly - `cleanup_qemu_files` is idempotent.
pub fn cleanup_runtime_files() {
    if let Some(dir) = SHUTDOWN_SOCK_DIR.get() {
        cleanup_qemu_files(dir);
    }
}

/// Remove the runtime directory and everything inside it. Idempotent - safe
/// to call when the directory does not exist. This is the upstream macOS
/// final-cleanup contract used by normal exit, signals, and atexit.
pub fn cleanup_qemu_files(sock_dir: &Path) {
    cleanup_macos_qemu_files_inner(sock_dir, /* keep_vmnet = */ false);
}

/// Restart-time cleanup: removes QEMU-managed transient files but preserves
/// `vmnet-*.sock`, which is managed by `SocketVmnet`'s own lifetime and must
/// outlive QEMU restarts (its daemon would exit if the socket vanished).
pub fn cleanup_qemu_files_for_restart(sock_dir: &Path) {
    cleanup_macos_qemu_files_inner(sock_dir, /* keep_vmnet = */ true);
}

fn cleanup_macos_qemu_files_inner(sock_dir: &Path, keep_vmnet: bool) {
    // Zero the shm magic *before* unlinking so any live reader (e.g. the UI's
    // main_stream poll loop) sees magic=0 through its existing mmap and drops
    // its mapping instead of staying stuck on the dead inode after restart.
    for shm_name in &["main.shm", "jog.shm"] {
        let shm = sock_dir.join(shm_name);
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&shm) {
            use std::io::Write;
            let _ = f.write_all(&[0u8; 4]);
        }
    }
    if let Ok(entries) = std::fs::read_dir(sock_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if keep_vmnet && entry.file_name().to_string_lossy().starts_with("vmnet-") {
                continue;
            }
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    let _ = std::fs::remove_dir_all(&path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
    if !keep_vmnet {
        let _ = std::fs::remove_dir(sock_dir);
    }
}

/// Linux restart/drop cleanup removes only launch-owned transports and shared
/// memory. The run directory and serial logs remain available to operators.
#[cfg(target_os = "linux")]
fn cleanup_linux_qemu_files_for_restart(run_dir: &Path) {
    for shm_name in &["main.shm", "jog.shm", "ram.shm"] {
        let shm = run_dir.join(shm_name);
        if let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(&shm) {
            use std::io::Write;
            let _ = file.write_all(&[0u8; 4]);
        }
        let _ = std::fs::remove_file(shm);
    }
    for name in [
        "ctrl.sock",
        "cfg.sock",
        "usb.placeholder",
        "usb.empty.medium",
    ] {
        let _ = std::fs::remove_file(run_dir.join(name));
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::{
        acquire_instance_lock, cleanup_linux_qemu_files_for_restart, remove_stale_qmp_sockets,
        InstanceError,
    };
    use super::{cleanup_qemu_files, cleanup_qemu_files_for_restart};
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestRunDir(PathBuf);

    impl TestRunDir {
        fn new() -> Self {
            loop {
                let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
                let dir = std::env::temp_dir().join(format!(
                    "cdj3k-instance-test-{}-{}-{sequence}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                match std::fs::create_dir(&dir) {
                    Ok(()) => return Self(dir),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("failed to create test run directory: {error}"),
                }
            }
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestRunDir {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.0) {
                assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn instance_lock_blocks_parallel_launch_and_allows_restart_after_release() {
        let run_dir = TestRunDir::new();
        let first = acquire_instance_lock(run_dir.path()).unwrap();
        assert!(matches!(
            acquire_instance_lock(run_dir.path()),
            Err(InstanceError::InstanceLocked(_))
        ));
        drop(first);
        let second = acquire_instance_lock(run_dir.path()).unwrap();
        assert!(run_dir.path().join(".instance.lock").is_file());
        drop(second);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stale_linux_qmp_socket_is_removed_without_erasing_diagnostics() {
        let run_dir = TestRunDir::new();
        let stale = run_dir.path().join("qmp-previous.sock");
        std::fs::write(&stale, b"stale endpoint").unwrap();
        std::fs::write(run_dir.path().join("serial-previous.log"), b"diagnostic").unwrap();

        let lock = acquire_instance_lock(run_dir.path()).unwrap();
        remove_stale_qmp_sockets(run_dir.path());
        assert!(!stale.exists());
        assert_eq!(
            std::fs::read(run_dir.path().join("serial-previous.log")).unwrap(),
            b"diagnostic"
        );
        drop(lock);
    }

    #[test]
    fn macos_restart_cleanup_preserves_vmnet_and_removes_everything_else() {
        let run_dir = TestRunDir::new();
        for name in [
            "main.shm",
            "jog.shm",
            "ram.shm",
            "ctrl.sock",
            "cfg.sock",
            "usb.empty.medium",
            "vmnet-1.sock",
            "serial-previous.log",
        ] {
            std::fs::write(run_dir.path().join(name), b"data").unwrap();
        }
        std::fs::create_dir(run_dir.path().join("nested")).unwrap();
        std::fs::write(run_dir.path().join("nested/evidence"), b"data").unwrap();
        let mut old_main = std::fs::File::open(run_dir.path().join("main.shm")).unwrap();

        cleanup_qemu_files_for_restart(run_dir.path());
        for name in [
            "main.shm",
            "jog.shm",
            "ram.shm",
            "ctrl.sock",
            "cfg.sock",
            "usb.empty.medium",
            "serial-previous.log",
            "nested",
        ] {
            assert!(
                !run_dir.path().join(name).exists(),
                "{name} survived restart cleanup"
            );
        }
        assert!(run_dir.path().is_dir());
        assert!(run_dir.path().join("vmnet-1.sock").exists());
        let mut magic = [1u8; 4];
        old_main.read_exact(&mut magic).unwrap();
        assert_eq!(magic, [0; 4]);
    }

    #[test]
    fn macos_final_cleanup_removes_the_entire_runtime_directory() {
        let run_dir = TestRunDir::new();
        for name in ["main.shm", "jog.shm", "vmnet-1.sock", "serial.log"] {
            std::fs::write(run_dir.path().join(name), b"data").unwrap();
        }
        std::fs::create_dir(run_dir.path().join("nested")).unwrap();
        std::fs::write(run_dir.path().join("nested/evidence"), b"data").unwrap();

        cleanup_qemu_files(run_dir.path());
        assert!(!run_dir.path().exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_restart_cleanup_preserves_run_directory_and_diagnostics() {
        let run_dir = TestRunDir::new();
        for name in [
            "main.shm",
            "jog.shm",
            "ram.shm",
            "ctrl.sock",
            "cfg.sock",
            "usb.placeholder",
            "usb.empty.medium",
            "qmp-current.sock",
            "vmnet-1.sock",
            "serial-previous.log",
            ".instance.lock",
        ] {
            std::fs::write(run_dir.path().join(name), b"data").unwrap();
        }

        cleanup_linux_qemu_files_for_restart(run_dir.path());
        for name in [
            "main.shm",
            "jog.shm",
            "ram.shm",
            "ctrl.sock",
            "cfg.sock",
            "usb.placeholder",
            "usb.empty.medium",
        ] {
            assert!(!run_dir.path().join(name).exists(), "{name} survived");
        }
        assert!(run_dir.path().is_dir());
        assert!(run_dir.path().join("qmp-current.sock").exists());
        assert!(run_dir.path().join("vmnet-1.sock").exists());
        assert!(run_dir.path().join("serial-previous.log").exists());
        assert!(run_dir.path().join(".instance.lock").exists());
    }
}
