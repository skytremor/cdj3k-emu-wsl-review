use std::path::PathBuf;

/// Platform-neutral guest inputs shared by the macOS and Linux launchers.
/// The Linux launcher owns the QEMU executable and host backend choices; this
/// value only describes the guest topology and its per-instance state.
#[derive(Clone, Debug)]
pub struct GuestRuntimeConfig {
    pub instance_id: u32,
    pub kernel: PathBuf,
    pub initramfs: PathBuf,
    pub emmc_img: Option<PathBuf>,
    pub run_dir: PathBuf,
    pub qmp_port: u16,
    pub ssh_port: u16,
    pub mac: String,
    pub service_mode: bool,
}

impl GuestRuntimeConfig {
    pub fn main_shm_path(&self) -> PathBuf {
        self.run_dir.join("main.shm")
    }

    pub fn jog_shm_path(&self) -> PathBuf {
        self.run_dir.join("jog.shm")
    }
}

/// Linux/WSL host-side QEMU configuration.  Keeping this separate from
/// [`QemuConfig`] makes the existing macOS HVF/FFI defaults unchanged.
#[derive(Clone, Debug)]
pub struct LinuxQemuConfig {
    pub guest: GuestRuntimeConfig,
    pub qemu: PathBuf,
    pub qemu_img: PathBuf,
    pub audio: bool,
    pub audio_device: Option<String>,
    pub network: bool,
    pub virtual_media: bool,
    /// Optional launch-scoped serial log. `spawn_linux` supplies a unique
    /// path when this is `None`; callers may provide one for deterministic
    /// tests or operator tooling.
    pub serial_log: Option<PathBuf>,
    /// Launch-unique QMP socket. Linux fills this immediately before spawn;
    /// `None` keeps argv characterization tests deterministic.
    pub qmp_socket: Option<PathBuf>,
}

impl LinuxQemuConfig {
    pub fn build_argv(&self) -> Vec<String> {
        let g = &self.guest;
        let serial_log = self
            .serial_log
            .clone()
            .unwrap_or_else(|| g.run_dir.join("serial.log"));
        let mut args = vec![
            "-machine".into(),
            "virt,gic-version=3".into(),
            "-global".into(),
            "virtio-mmio.force-legacy=false".into(),
            "-accel".into(),
            "tcg,thread=multi".into(),
            "-cpu".into(),
            "cortex-a72".into(),
            "-smp".into(),
            "4".into(),
            "-m".into(),
            "4038197248B".into(),
            "-kernel".into(),
            g.kernel.display().to_string(),
            "-initrd".into(),
            g.initramfs.display().to_string(),
            "-append".into(),
            linux_kernel_command_line(g, self.audio),
            "-display".into(),
            format!("shm,path={}", g.main_shm_path().display()),
            "-qmp".into(),
            format!(
                "unix:{},server=on,wait=off",
                self.qmp_socket
                    .clone()
                    .unwrap_or_else(|| g.run_dir.join("qmp.sock"))
                    .display()
            ),
            "-serial".into(),
            format!("file:{}", serial_log.display()),
            "-monitor".into(),
            "none".into(),
            "-no-reboot".into(),
        ];

        args.extend([
            "-object".into(),
            format!(
                "memory-backend-file,id=jogshm,mem-path={},size=1M,share=on",
                g.jog_shm_path().display()
            ),
            "-device".into(),
            "ivshmem-plain,memdev=jogshm,master=on".into(),
            "-device".into(),
            "virtio-gpu-device,id=virtio-gpu0,xres=1280,yres=720,max_outputs=2".into(),
            "-device".into(),
            "virtio-serial-device,max_ports=8".into(),
        ]);

        for name in ["ctrl", "cfg"] {
            args.extend([
                "-chardev".into(),
                format!(
                    "socket,id=vserial_{name},path={},server=on,wait=off",
                    g.run_dir.join(format!("{name}.sock")).display()
                ),
                "-device".into(),
                format!("virtserialport,chardev=vserial_{name},name=cdj3k.{name}"),
            ]);
        }

        if let Some(emmc) = &g.emmc_img {
            args.extend([
                "-drive".into(),
                format!(
                    "file={},if=none,id=emmc0,format=qcow2,cache=writeback,file.locking=off",
                    emmc.display()
                ),
                "-device".into(),
                "virtio-blk-device,drive=emmc0,id=emmc0".into(),
            ]);
        }

        if self.virtual_media {
            let placeholder = g.run_dir.join("usb.empty.medium");
            args.extend([
                "-drive".into(),
                format!(
                    "file={},if=none,id=usb0,format=raw,cache=writeback,file.locking=off",
                    placeholder.display()
                ),
                "-device".into(),
                "virtio-blk-device,drive=usb0,id=usb0".into(),
            ]);
        }

        if self.audio {
            let mut audio = "pa,id=audio0".to_string();
            if let Some(device) = self.audio_device.as_deref().filter(|v| !v.is_empty()) {
                audio.push_str(",out.name=");
                audio.push_str(device);
            }
            args.extend([
                "-audiodev".into(),
                audio,
                "-device".into(),
                "virtio-sound-device,audiodev=audio0,streams=1".into(),
            ]);
        }

        if self.network {
            args.extend([
                "-netdev".into(),
                format!("user,id=net0,hostfwd=tcp::{}-:22", g.ssh_port),
                "-device".into(),
                format!("virtio-net-device,netdev=net0,mac={}", g.mac),
            ]);
        }
        args
    }
}

fn linux_kernel_command_line(guest: &GuestRuntimeConfig, audio: bool) -> String {
    let mut cmd =
        "root=/dev/ram0 rdinit=/init loglevel=7 nowatchdog rng_core.default_quality=1024 console=ttyAMA0,115200 systemd.journald.forward_to_console=1 virtio_gpu.modeset=1".to_string();
    if guest.service_mode {
        cmd.push_str(" subucom_testmode");
    }
    if audio {
        cmd.push_str(" snd-dummy.enable=0");
    }
    cmd
}

#[cfg(test)]
mod linux_tests {
    use super::*;

    fn config() -> LinuxQemuConfig {
        LinuxQemuConfig {
            guest: GuestRuntimeConfig {
                instance_id: 2,
                kernel: "/tmp/Image".into(),
                initramfs: "/tmp/initramfs".into(),
                emmc_img: Some("/tmp/emmc.qcow2".into()),
                run_dir: "/tmp/cdj3k-instance-2".into(),
                qmp_port: 4447,
                ssh_port: 2224,
                mac: "0a:00:00:00:00:02".into(),
                service_mode: false,
            },
            qemu: "qemu-system-aarch64".into(),
            qemu_img: "qemu-img".into(),
            audio: false,
            audio_device: None,
            network: true,
            virtual_media: false,
            serial_log: None,
            qmp_socket: None,
        }
    }

    #[test]
    fn linux_defaults_use_tcg_and_two_display_outputs() {
        let args = config().build_argv();
        assert_eq!(
            args,
            [
                "-machine",
                "virt,gic-version=3",
                "-global",
                "virtio-mmio.force-legacy=false",
                "-accel",
                "tcg,thread=multi",
                "-cpu",
                "cortex-a72",
                "-smp",
                "4",
                "-m",
                "4038197248B",
                "-kernel",
                "/tmp/Image",
                "-initrd",
                "/tmp/initramfs",
                "-append",
                "root=/dev/ram0 rdinit=/init loglevel=7 nowatchdog rng_core.default_quality=1024 console=ttyAMA0,115200 systemd.journald.forward_to_console=1 virtio_gpu.modeset=1",
                "-display",
                "shm,path=/tmp/cdj3k-instance-2/main.shm",
                "-qmp",
                "unix:/tmp/cdj3k-instance-2/qmp.sock,server=on,wait=off",
                "-serial",
                "file:/tmp/cdj3k-instance-2/serial.log",
                "-monitor",
                "none",
                "-no-reboot",
                "-object",
                "memory-backend-file,id=jogshm,mem-path=/tmp/cdj3k-instance-2/jog.shm,size=1M,share=on",
                "-device",
                "ivshmem-plain,memdev=jogshm,master=on",
                "-device",
                "virtio-gpu-device,id=virtio-gpu0,xres=1280,yres=720,max_outputs=2",
                "-device",
                "virtio-serial-device,max_ports=8",
                "-chardev",
                "socket,id=vserial_ctrl,path=/tmp/cdj3k-instance-2/ctrl.sock,server=on,wait=off",
                "-device",
                "virtserialport,chardev=vserial_ctrl,name=cdj3k.ctrl",
                "-chardev",
                "socket,id=vserial_cfg,path=/tmp/cdj3k-instance-2/cfg.sock,server=on,wait=off",
                "-device",
                "virtserialport,chardev=vserial_cfg,name=cdj3k.cfg",
                "-drive",
                "file=/tmp/emmc.qcow2,if=none,id=emmc0,format=qcow2,cache=writeback,file.locking=off",
                "-device",
                "virtio-blk-device,drive=emmc0,id=emmc0",
                "-netdev",
                "user,id=net0,hostfwd=tcp::2224-:22",
                "-device",
                "virtio-net-device,netdev=net0,mac=0a:00:00:00:00:02",
            ]
        );
    }

    #[test]
    fn optional_audio_network_and_media_are_independent() {
        let mut config = config();
        config.audio = true;
        config.network = false;
        config.virtual_media = true;
        let args = config.build_argv();
        assert!(args.iter().any(|arg| arg.contains("virtio-sound-device")));
        assert!(!args.iter().any(|arg| arg.contains("-netdev")));
        assert!(args.iter().any(|arg| arg.contains("id=usb0")));
        assert!(args.iter().any(|arg| arg.contains("id=emmc0")));
    }

    #[test]
    fn explicit_launch_paths_and_service_mode_are_forwarded() {
        let mut config = config();
        config.guest.service_mode = true;
        config.guest.emmc_img = None;
        config.serial_log = Some("/tmp/launch-42.log".into());
        config.qmp_socket = Some("/tmp/launch-42.sock".into());
        config.audio = true;
        config.audio_device = Some("sink-1".into());
        let args = config.build_argv();

        assert!(args.windows(2).any(|pair| pair
            == [
                "-append",
                "root=/dev/ram0 rdinit=/init loglevel=7 nowatchdog rng_core.default_quality=1024 console=ttyAMA0,115200 systemd.journald.forward_to_console=1 virtio_gpu.modeset=1 subucom_testmode snd-dummy.enable=0"
            ]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-qmp", "unix:/tmp/launch-42.sock,server=on,wait=off"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-serial", "file:/tmp/launch-42.log"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-audiodev", "pa,id=audio0,out.name=sink-1"]));
        assert!(!args.iter().any(|arg| arg.contains("id=emmc0")));
    }
}

/// Configuration for a single QEMU CDJ-3000 instance.
#[derive(Clone, Debug)]
pub struct QemuConfig {
    /// Instance index - selects /tmp/cdj3k-emu/instance-{id}/ socket directory.
    pub instance_id: u32,

    /// aarch64 kernel image.
    pub kernel: PathBuf,

    /// Initramfs image (initramfs-patched.cpio.gz).
    pub initramfs: PathBuf,

    /// Use HVF acceleration.
    pub hvf: bool,

    /// Back guest RAM with a MAP_SHARED file for host-side mmap injection.
    pub shm: bool,

    /// Expose virtio-sound-device (CoreAudio backend).
    pub audio: bool,

    /// CoreAudio device UID (kAudioDevicePropertyDeviceUID) to bind the
    /// output stream to. `None` means "follow system default output".
    /// Forwarded to QEMU as `out.device-uid=<UID>` on `-audiodev coreaudio`.
    /// Only meaningful when `audio == true`.
    pub audio_device_uid: Option<String>,

    /// Boot into EP122 service/test mode.
    pub service_mode: bool,

    /// eMMC qcow2 image.
    /// virtio_blk.c maps device index 0 → /dev/mmcblk1 (major 179, base minor 8).
    /// Partitions p1..p8 appear as mmcblk1p1..mmcblk1p8.
    /// p7 = settings (/home/root/settings), p8 = user data (/mnt).
    pub emmc_img: Option<PathBuf>,

    /// socket_vmnet Unix socket for bridged Pro DJ Link on physical interfaces.
    /// QEMU connects via `-netdev stream,addr.type=unix,addr.path=<sock>`.
    pub net_socket_vmnet: Option<PathBuf>,

    /// TAP interface name - informational only (e.g. for logs/display).
    pub net_tap_iface: Option<String>,

    /// Open file descriptor for the QEMU-side TAP device.
    /// When set, QEMU receives `-netdev tap,fd=<N>` instead of `ifname=`.
    /// The fd must have FD_CLOEXEC cleared before exec.
    pub net_tap_fd: Option<i32>,

    /// QMP TCP port.  Default: 4445 + instance_id (so multiple instances can coexist).
    pub qmp_port: u16,

    /// GDB TCP port.  Default: 1235 + instance_id.
    pub gdb_port: u16,

    /// SSH port forwarded from guest :22.  Default: 2222 + instance_id.
    pub ssh_port: u16,

    /// Persistent MAC for the guest virtio-net device. When `None`, falls back
    /// to the deterministic `0a:00:00:00:00:{instance_id}`.
    pub mac: Option<String>,

    /// When `true`, QEMU writes the guest serial console to
    /// `{sock_dir}/serial.log`. Disabled by default in release builds - the
    /// file grows unbounded over a long session and is only useful when
    /// debugging boot / kernel panics. Enable from the CLI with `--serial-log`
    /// or by setting `CDJ3K_SERIAL_LOG=1`.
    pub serial_log: bool,
}

impl QemuConfig {
    /// Guest RAM in bytes. Anonymous mmap is demand-paged on macOS HVF, so the
    /// host only commits pages the guest actually touches - 1.5 GiB is plenty
    /// for the observed working set and lets four instances coexist
    /// comfortably. Must be 64 KiB aligned.
    pub const MEM_BYTES: u64 = 0x6000_0000;

    pub fn new(kernel: PathBuf, initramfs: PathBuf) -> Self {
        Self {
            instance_id: 0,
            kernel,
            initramfs,
            hvf: true,
            shm: false,
            audio: false,
            audio_device_uid: None,
            service_mode: false,
            emmc_img: None,
            net_socket_vmnet: None,
            net_tap_iface: None,
            net_tap_fd: None,
            qmp_port: 4445,
            gdb_port: 1235,
            ssh_port: 2222,
            mac: None,
            serial_log: false,
        }
    }

    /// Socket directory: see [`runtime_paths::instance_dir`].
    pub fn sock_dir(&self) -> PathBuf {
        cdj3k_emu_platform::runtime_paths::instance_dir(self.instance_id)
    }

    /// Shared RAM file path (only when shm=true).
    pub fn shm_path(&self) -> PathBuf {
        self.sock_dir().join("ram.shm")
    }

    /// ivshmem-backed file for the jog-LCD zero-copy frame buffer.
    /// 1 MiB; layout matches `JOG_SHM_*` in `guest/shim/shim.h` and
    /// `crates/cdj3k-emu-streams/src/jog_stream.rs`.
    pub fn jog_shm_path(&self) -> PathBuf {
        self.sock_dir().join("jog.shm")
    }

    /// Placeholder backing file for the always-present USB virtio-blk slot.
    /// Created by QemuInstance::spawn; swapped live via QMP blockdev-change-medium.
    pub fn usb_placeholder_path(&self) -> PathBuf {
        self.sock_dir().join("usb.placeholder")
    }

    /// Build the argv list to pass to cdj3k_emu_qemu_run.
    pub fn build_argv(&self) -> Vec<String> {
        let sock = self.sock_dir();

        // GIC selection:
        //   macOS 15+ (Sequoia) with HVF → in-kernel vGIC via hv_gic_create.
        //     Drops per-IRQ vCPU-exit cost dramatically; the dominant
        //     cost driver for HVF guests with chatty workloads (EP122).
        //   macOS 13/14 or TCG       → userspace GIC emulation.
        //     Slower per-IRQ but functional - no Apple-side hypervisor
        //     primitive exists on those releases.
        // Requires QEMU master post-2026-05-05 for the in-kernel path.
        let in_kernel_gic = self.hvf && cdj3k_emu_platform::host::has_hvf_in_kernel_gic();
        let machine_base = if in_kernel_gic {
            "virt,gic-version=3,kernel-irqchip=on"
        } else {
            "virt,gic-version=3,kernel-irqchip=off"
        };

        let mut args: Vec<String> = vec![
            "cdj3k-emu-qemu".into(),
            "-machine".into(),
            machine_base.into(),
            "-m".into(),
            format!("{}B", Self::MEM_BYTES),
        ];

        if self.hvf {
            args.extend(["-accel".into(), "hvf".into()]);
            args.extend(["-cpu".into(), "host".into()]);
        } else {
            args.extend(["-cpu".into(), "cortex-a72".into()]);
        }

        args.extend(["-smp".into(), "4".into()]);

        args.extend(["-kernel".into(), self.kernel.display().to_string()]);
        args.extend(["-initrd".into(), self.initramfs.display().to_string()]);

        let mut kcmd =
            "root=/dev/ram0 rdinit=/init loglevel=7 nowatchdog rng_core.default_quality=1024"
                .to_string();
        // PL011 UART console works on vanilla 6.6 - always wire it up.
        kcmd.push_str(" console=ttyAMA0,115200");
        // Forward journal to console so EP122 crash details appear in the serial log.
        kcmd.push_str(" systemd.journald.forward_to_console=1");
        // virtio_gpu_init() requires VIRTIO_F_VERSION_1; force-legacy=off on the
        // MMIO transport ensures that bit is negotiated.
        kcmd.push_str(" virtio_gpu.modeset=1");
        if self.service_mode {
            kcmd.push_str(" subucom_testmode");
        }
        // snd-dummy is built-in (CONFIG_SND_DUMMY=y) so its card always
        // auto-registers first - and JUCE picks card 0. When the real
        // virtio-snd path is wired up, disable Dummy so JUCE opens vsnd
        // (which becomes card 0 when Dummy is silent).
        if self.audio {
            kcmd.push_str(" snd-dummy.enable=0");
        }
        args.extend(["-append".into(), kcmd]);

        if self.serial_log {
            let serial_log = self.sock_dir().join("serial.log").display().to_string();
            args.extend(["-serial".into(), format!("file:{}", serial_log)]);
        } else {
            // No serial sink: also suppress QEMU's default monitor and
            // parallel chardevs. Otherwise the default-monitor allocator
            // creates a hidden QemuTextConsole that arms the VT100 cursor
            // blink timer (~250 ms self-rearm), and each fire walks the
            // glyph cache under BQL - measured at 0.4-0.7% of the audio
            // refill thread on a two-instance setup. Empty defaults =
            // no text consoles created = no cursor work.
            args.extend([
                "-serial".into(),
                "null".into(),
                "-monitor".into(),
                "none".into(),
                "-parallel".into(),
                "none".into(),
            ]);
        }

        if self.shm {
            let path = self.shm_path();
            args.extend([
                "-object".into(),
                format!(
                    "memory-backend-file,id=ram0,size={}B,mem-path={},share=on",
                    Self::MEM_BYTES,
                    path.display()
                ),
                "-machine".into(),
                format!("{},memory-backend=ram0", machine_base),
            ]);
        }

        args.extend([
            "-display".into(),
            format!("shm,path={}/main.shm", sock.display()),
        ]);
        args.extend([
            "-qmp".into(),
            format!("tcp:localhost:{},server=on,wait=off", self.qmp_port),
        ]);

        args.extend([
            "-object".into(),
            "rng-random,id=rng0,filename=/dev/urandom".into(),
            "-device".into(),
            "virtio-rng-device,rng=rng0".into(),
        ]);

        if self.audio {
            // Build the audiodev string. When the user pinned a specific
            // device via the per-instance Audio Output picker, append
            // `out.device-uid=<UID>` so the patched coreaudio.m binds to
            // that device instead of the system default output.
            // out.buffer-length=5000 (5 ms): a previous bump to 30000 us
            // did not help the audible pops because Apple Silicon's
            // built-in HAL buffer-frame-size range clamped the request
            // down to ~13.78 ms regardless, AND the underlying cause is
            // not HAL-side scheduling jitter but guest VCPU starvation
            // under host CPU/GPU stress. Reverted to 5 ms so we don't
            // pay latency we don't get any value for. Real fix is RT
            // scheduling for the HVF VCPU threads (see hvf-accel-ops
            // patch) so the guest's PCM thread can't be preempted.
            let mut audiodev =
                String::from("coreaudio,id=audio0,in.voices=0,out.buffer-length=5000");
            if let Some(uid) = self.audio_device_uid.as_deref().filter(|s| !s.is_empty()) {
                // CoreAudio UIDs contain ':', spaces, and other chars QEMU's
                // -audiodev parser passes through untouched (it splits on
                // ',' and '=' only); no escaping needed in practice.
                audiodev.push_str(",out.device-uid=");
                audiodev.push_str(uid);
            }
            args.extend([
                "-audiodev".into(),
                audiodev,
                "-device".into(),
                "virtio-sound-device,audiodev=audio0,streams=1".into(),
            ]);
        }
        // When disabled: no virtio-sound device. The guest auto-loads
        // snd-dummy so JUCE still enumerates an ALSA card without
        // generating any guest↔host audio traffic.

        let mac = self
            .mac
            .clone()
            .unwrap_or_else(|| format!("0a:00:00:00:00:{:02x}", self.instance_id & 0xff));
        if let Some(fd) = self.net_tap_fd {
            args.extend([
                "-netdev".into(),
                format!("tap,id=net0,fd={}", fd),
                "-device".into(),
                format!("virtio-net-device,netdev=net0,mac={},mrg_rxbuf=off", mac),
            ]);
        } else if let Some(sock) = &self.net_socket_vmnet {
            args.extend([
                "-netdev".into(),
                format!(
                    "stream,id=net0,server=off,addr.type=unix,addr.path={}",
                    sock.display()
                ),
                "-device".into(),
                format!("virtio-net-device,netdev=net0,mac={},mrg_rxbuf=off", mac),
            ]);
        } else {
            args.extend([
                "-netdev".into(),
                format!("user,id=net0,hostfwd=tcp::{}-:22", self.ssh_port),
                "-device".into(),
                "virtio-net-device,netdev=net0,mrg_rxbuf=off".into(),
            ]);
        }

        args.extend(["-device".into(), "virtio-serial-device,max_ports=8".into()]);
        for (name, nr) in &[
            ("ctrl", None::<u32>), // bidirectional: subucom_forwarder bridges subucom_ctrl ↔ host
            ("cfg", None),         // bidirectional: cdj3k-cfgd ↔ host runtime
                                   //   host→guest: usb attach|detach, set/get sysfs params
                                   //   guest→host: usb_state, param values, latency every 3s
        ] {
            let sock_path = sock.join(format!("{}.sock", name));
            args.extend([
                "-chardev".into(),
                format!(
                    "socket,id=vserial_{},path={},server=on,wait=off",
                    name,
                    sock_path.display()
                ),
            ]);
            let mut dev = format!(
                "virtserialport,chardev=vserial_{},name=cdj3k.{}",
                name, name
            );
            if let Some(n) = nr {
                dev.push_str(&format!(",nr={}", n));
            }
            args.extend(["-device".into(), dev]);
        }

        // ivshmem-plain (jog LCD zero-copy frame buffer). The guest's
        // ep122_shim.so writes extracted 320×240 XRGB pixels directly into
        // BAR2; the host mmaps `jog.shm` and polls the seqlock counter.
        args.extend([
            "-object".into(),
            format!(
                "memory-backend-file,id=jogshm,mem-path={},size=1M,share=on",
                self.jog_shm_path().display()
            ),
            "-device".into(),
            "ivshmem-plain,memdev=jogshm,master=on".into(),
        ]);

        // Pioneer virtio_blk.c probes in reverse virtio-mmio slot order (last listed = first
        // probed).  USB is listed BEFORE eMMC so the indices work out:
        //   usb0  (listed first  → probed second → index 1 → /dev/sdb)
        //   emmc0 (listed second → probed first  → index 0 → /dev/mmcblk1)
        //
        // The USB slot is always present, initially backed by a 1-sector placeholder.
        // UsbManager hot-swaps the medium via QMP blockdev-change-medium without
        // restarting QEMU; virtio_blk_change_media fires virtio_notify_config which
        // triggers Pioneer's virtblk_config_changed → revalidate_disk on the guest.
        // file.locking=off - qcow2/raw open uses fcntl byte-range locks at offset
        // 100; if a prior QEMU subprocess hasn't fully released its FD when we
        // restart, the new process aborts with "Failed to lock byte 100". We
        // gate exclusive access at the host (.app) level instead.
        args.extend([
            "-drive".into(),
            format!(
                "file={},if=none,id=usb0,format=raw,cache=writeback,file.locking=off",
                self.usb_placeholder_path().display()
            ),
            "-device".into(),
            "virtio-blk-device,drive=usb0,id=usb0".into(),
        ]);

        if let Some(img) = &self.emmc_img {
            args.extend([
                "-drive".into(),
                format!(
                    "file={},if=none,id=emmc0,format=qcow2,cache=writeback,file.locking=off",
                    img.display()
                ),
                "-device".into(),
                "virtio-blk-device,drive=emmc0,id=emmc0".into(),
            ]);
        }

        args.extend([
            "-device".into(),
            "virtio-gpu-device,id=virtio-gpu0,xres=1280,yres=720,max_outputs=1".into(),
        ]);

        args.extend(["-no-reboot".into()]);
        args.extend(["-gdb".into(), format!("tcp::{}", self.gdb_port)]);

        args
    }
}

#[cfg(test)]
mod tests {
    use super::QemuConfig;
    use std::path::PathBuf;

    fn config() -> QemuConfig {
        QemuConfig::new(
            PathBuf::from("/build/Image"),
            PathBuf::from("/build/initramfs"),
        )
    }

    fn has_arg(args: &[String], value: &str) -> bool {
        args.iter().any(|arg| arg == value)
    }

    fn arg_starting_with<'a>(args: &'a [String], prefix: &str) -> &'a str {
        args.iter()
            .find(|arg| arg.starts_with(prefix))
            .map(String::as_str)
            .unwrap_or_else(|| panic!("missing argument starting with {prefix:?}"))
    }

    #[test]
    fn defaults_preserve_the_macos_hvf_command_shape() {
        let args = config().build_argv();

        assert_eq!(args[0], "cdj3k-emu-qemu");
        assert!(has_arg(&args, "-accel"));
        assert!(has_arg(&args, "hvf"));
        assert!(has_arg(&args, "-cpu"));
        assert!(has_arg(&args, "host"));
        assert!(has_arg(&args, "-smp"));
        assert!(has_arg(&args, "4"));
        assert!(has_arg(&args, "-kernel"));
        assert!(has_arg(&args, "/build/Image"));
        assert!(has_arg(&args, "-initrd"));
        assert!(has_arg(&args, "/build/initramfs"));
        assert!(has_arg(&args, "-display"));
        assert!(arg_starting_with(&args, "shm,path=").ends_with("/main.shm"));
        assert!(has_arg(
            &args,
            "virtio-gpu-device,id=virtio-gpu0,xres=1280,yres=720,max_outputs=1"
        ));
        assert!(has_arg(
            &args,
            "virtio-net-device,netdev=net0,mrg_rxbuf=off"
        ));
        assert!(!has_arg(&args, "-audiodev"));
        assert!(!args
            .iter()
            .any(|arg| arg.starts_with("file=/") && arg.contains("emmc")));
    }

    #[test]
    fn tcg_mode_uses_the_explicit_portable_cpu_and_gic() {
        let mut cfg = config();
        cfg.hvf = false;

        let args = cfg.build_argv();

        assert!(has_arg(&args, "-cpu"));
        assert!(has_arg(&args, "cortex-a72"));
        assert!(!has_arg(&args, "-accel"));
        assert!(arg_starting_with(&args, "virt,gic-version=3,").ends_with("kernel-irqchip=off"));
    }

    #[test]
    fn optional_devices_are_independent_and_use_explicit_paths() {
        let mut cfg = config();
        cfg.hvf = false;
        cfg.audio = true;
        cfg.audio_device_uid = Some("Built-in Output,uid".into());
        cfg.service_mode = true;
        cfg.emmc_img = Some(PathBuf::from("/state/emmc.qcow2"));
        cfg.instance_id = 2;
        cfg.ssh_port = 2224;

        let args = cfg.build_argv();
        let append = arg_starting_with(&args, "root=/dev/ram0");

        assert!(append.contains("subucom_testmode"));
        assert!(append.contains("snd-dummy.enable=0"));
        assert!(has_arg(&args, "-audiodev"));
        assert!(
            arg_starting_with(&args, "coreaudio,").contains("out.device-uid=Built-in Output,uid")
        );
        assert!(has_arg(
            &args,
            "virtio-sound-device,audiodev=audio0,streams=1"
        ));
        assert!(args
            .iter()
            .any(|arg| arg.contains("file=/state/emmc.qcow2")));
        assert!(args.iter().any(|arg| arg.contains("instance-2")));
        assert!(arg_starting_with(&args, "user,id=net0,").contains("hostfwd=tcp::2224-:22"));
    }

    #[test]
    fn serial_logging_is_opt_in() {
        let mut cfg = config();
        let quiet = cfg.build_argv();
        assert!(has_arg(&quiet, "null"));
        assert!(has_arg(&quiet, "none"));

        cfg.serial_log = true;
        let logged = cfg.build_argv();
        assert!(logged
            .iter()
            .any(|arg| arg.starts_with("file:") && arg.ends_with("/serial.log")));
    }
}
