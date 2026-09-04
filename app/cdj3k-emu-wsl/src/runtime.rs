use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use std::{path::Path, process::Command};

use cdj3k_emu_platform::menu_state::{self, APP_SHUTDOWN};
use cdj3k_emu_runtime::register_worker_thread;
use cdj3k_emu_runtime::{CfgClient, LinuxQemuConfig, QemuInstance};
use cdj3k_emu_streams::{ControlConnectionGate, LaunchEpoch};

pub fn spawn(config: LinuxQemuConfig, start: bool, control_gate: Arc<ControlConnectionGate>) {
    let handle = thread::Builder::new()
        .name("cdj3k-wsl-runtime".into())
        .spawn(move || run(config, start, control_gate))
        .expect("failed to start WSL runtime worker");
    register_worker_thread(handle);
}

fn run(mut config: LinuxQemuConfig, start: bool, control_gate: Arc<ControlConnectionGate>) {
    let mut instance = start_instance(&config, start, Arc::clone(&control_gate));
    let cfg_client = CfgClient::new(&config.guest.run_dir);
    let mut alc_pushed_for_boot = false;
    let mut last_audio_refresh = Instant::now() - Duration::from_secs(5);
    menu_state::refresh_audio_devices();

    while !APP_SHUTDOWN.load(Ordering::Relaxed) {
        let (
            audio_changed,
            audio_enabled,
            audio_device,
            service_mode,
            alc_toggle,
            alc_enabled,
            usb_create,
            media_mount,
            media_eject,
            media_path,
        ) = {
            let mut state = menu_state::lock();
            (
                std::mem::take(&mut state.audio_toggle_requested)
                    || std::mem::take(&mut state.audio_device_toggle_requested),
                state.audio_enabled,
                state.audio_device_uid.clone(),
                state.service_mode,
                std::mem::take(&mut state.alc_toggle_requested),
                state.alc_enabled,
                std::mem::take(&mut state.usb_create_req),
                std::mem::take(&mut state.usb_virtual_mount_req),
                std::mem::take(&mut state.usb_eject_req),
                state.usb_virtual_img.clone(),
            )
        };
        config.guest.service_mode = service_mode;

        if last_audio_refresh.elapsed() >= Duration::from_secs(5) {
            menu_state::refresh_audio_devices();
            last_audio_refresh = Instant::now();
        }

        if audio_changed {
            config.audio = audio_enabled;
            config.audio_device = audio_device;
            control_gate.invalidate();
            if let Some(mut old) = instance.take() {
                old.stop();
            }
            instance = start_instance(&config, true, Arc::clone(&control_gate));
            alc_pushed_for_boot = false;
        }

        if instance
            .as_ref()
            .is_some_and(|current| !current.is_running())
        {
            control_gate.invalidate();
            if let Some(mut dead) = instance.take() {
                let _ = dead.stop();
            }
        }

        if let Some(ref mut current) = instance {
            if usb_create {
                if !config.virtual_media {
                    eprintln!("cdj3k-emu-wsl: virtual media is disabled for this launch");
                } else if let Some(path) = media_path.as_ref() {
                    const DEFAULT_SIZE: u64 = 32_000_000_000;
                    match create_virtual_image(&config.qemu_img, path, DEFAULT_SIZE)
                        .and_then(|()| attach_virtual(&cfg_client, current, path))
                    {
                        Ok(()) => mark_virtual_mounted(config.guest.instance_id, path),
                        Err(error) => {
                            eprintln!("cdj3k-emu-wsl: USB create failed: {error}")
                        }
                    }
                }
            }
            if media_mount {
                if !config.virtual_media {
                    eprintln!("cdj3k-emu-wsl: virtual media is disabled for this launch");
                } else if let Some(path) = media_path.as_ref().filter(|path| path.exists()) {
                    match attach_virtual(&cfg_client, current, path) {
                        Ok(()) => mark_virtual_mounted(config.guest.instance_id, path),
                        Err(error) => {
                            eprintln!("cdj3k-emu-wsl: virtual media mount failed: {error}")
                        }
                    }
                }
            }
            if media_eject {
                if !config.virtual_media {
                    eprintln!("cdj3k-emu-wsl: virtual media is disabled for this launch");
                } else {
                    let placeholder = current_placeholder(&config);
                    match current
                        .qmp()
                        .blockdev_change_medium("usb0", &placeholder, "raw")
                    {
                        Ok(_) => {
                            menu_state::lock().usb_virtual_mounted = false;
                            persist_instance(config.guest.instance_id, |settings| {
                                settings.usb_virtual_path = None;
                            });
                        }
                        Err(error) => {
                            eprintln!("cdj3k-emu-wsl: virtual media eject failed: {error:?}")
                        }
                    }
                }
            }
        }

        if alc_toggle {
            let value = if alc_enabled { "1" } else { "0" };
            if let Err(error) = cfg_client.set_param("audio_sync_enabled", value) {
                eprintln!("cdj3k-emu-wsl: set audio_sync_enabled failed: {error}");
            }
            persist_instance(config.guest.instance_id, |settings| {
                settings.alc_enabled = alc_enabled;
            });
        }

        if !alc_pushed_for_boot && instance.is_some() && cfg_client.latency().is_some() {
            let value = if alc_enabled { "1" } else { "0" };
            if cfg_client.set_param("audio_sync_enabled", value).is_ok() {
                alc_pushed_for_boot = true;
            }
        }
        if let Some(latency) = cfg_client.latency() {
            menu_state::lock().latency_packed =
                menu_state::pack_latency(latency.total_ms, latency.guest_ms, latency.host_ms);
        }

        let action = {
            let mut state = menu_state::lock();
            if state.qemu_boot_requested {
                state.qemu_boot_requested = false;
                Some("start")
            } else if state.restart_requested {
                state.restart_requested = false;
                Some("restart")
            } else if state.stop_requested {
                state.stop_requested = false;
                Some("stop")
            } else {
                None
            }
        };

        match action {
            Some("start") if instance.is_none() => {
                instance = start_instance(&config, true, Arc::clone(&control_gate));
                alc_pushed_for_boot = false;
            }
            Some("restart") => {
                control_gate.invalidate();
                if let Some(mut old) = instance.take() {
                    old.stop();
                }
                instance = start_instance(&config, true, Arc::clone(&control_gate));
                alc_pushed_for_boot = false;
            }
            Some("stop") => {
                control_gate.invalidate();
                if let Some(mut old) = instance.take() {
                    old.stop();
                }
            }
            _ => {}
        }

        let running = instance.as_ref().is_some_and(QemuInstance::is_running);
        let mut state = menu_state::lock();
        state.qemu_running = running;
        state.application_started = control_gate.application_started();
        thread::sleep(Duration::from_millis(100));
    }

    if let Some(mut instance) = instance {
        instance.stop();
    }
    let mut state = menu_state::lock();
    state.qemu_running = false;
    state.application_started = false;
}

fn attach_virtual(
    cfg_client: &CfgClient,
    instance: &mut QemuInstance,
    path: &Path,
) -> std::io::Result<()> {
    instance
        .qmp()
        .blockdev_change_medium("usb0", &path.to_string_lossy(), "raw")
        .map_err(|error| std::io::Error::other(format!("QMP: {error:?}")))?;
    cfg_client.usb_attach()
}

fn create_virtual_image(qemu_img: &Path, path: &Path, size_bytes: u64) -> std::io::Result<()> {
    let status = Command::new(qemu_img)
        .args(["create", "-f", "raw"])
        .arg(path)
        .arg(size_bytes.to_string())
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "qemu-img create failed (exit {:?})",
            status.code()
        )));
    }
    for formatter in ["mkfs.exfat", "mkfs.vfat"] {
        let result = if formatter == "mkfs.vfat" {
            Command::new(formatter)
                .args(["-F", "32"])
                .arg(path)
                .status()
        } else {
            Command::new(formatter).arg(path).status()
        };
        match result {
            Ok(status) if status.success() => return Ok(()),
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no mkfs.exfat or mkfs.vfat formatter found",
    ))
}

fn mark_virtual_mounted(instance_id: u32, path: &Path) {
    {
        let mut state = menu_state::lock();
        state.usb_virtual_mounted = true;
    }
    persist_instance(instance_id, |settings| {
        settings.usb_virtual_path = Some(path.to_path_buf());
        settings.usb_physical_bsd = None;
    });
}

fn persist_instance<F>(instance_id: u32, mutate: F)
where
    F: FnOnce(&mut cdj3k_emu_storage::InstanceSettings),
{
    let mut settings = cdj3k_emu_storage::InstanceSettings::load_or_init(instance_id);
    mutate(&mut settings);
    if let Err(error) = settings.save(instance_id) {
        eprintln!("cdj3k-emu-wsl: persisting instance settings failed: {error}");
    }
}

fn start_instance(
    config: &LinuxQemuConfig,
    start: bool,
    control_gate: Arc<ControlConnectionGate>,
) -> Option<QemuInstance> {
    if !start {
        return None;
    }
    let epoch = control_gate.begin_launch();
    match QemuInstance::spawn_linux(config.clone()) {
        Ok(instance) => {
            menu_state::lock().qemu_running = true;
            spawn_serial_observer(
                instance.serial_log_path().to_path_buf(),
                control_gate,
                epoch,
            );
            Some(instance)
        }
        Err(error) => {
            control_gate.invalidate();
            eprintln!("cdj3k-emu-wsl: QEMU start failed: {error:?}");
            None
        }
    }
}

/// Follow only the unique serial file for one launch. This deliberately does
/// not inspect old logs or display readiness; the gate applies the current
/// launch token and main+jog readiness checks when the marker is found.
fn spawn_serial_observer(
    path: std::path::PathBuf,
    gate: Arc<ControlConnectionGate>,
    epoch: LaunchEpoch,
) {
    thread::Builder::new()
        .name("application-started-observer".into())
        .spawn(move || {
            const MARKER: &[u8] = b"Application Started";
            let mut offset = 0usize;
            let mut pending = Vec::new();
            loop {
                if !gate.is_current(epoch) {
                    return;
                }
                if let Ok(bytes) = std::fs::read(&path) {
                    if bytes.len() < offset {
                        offset = 0;
                        pending.clear();
                    }
                    if bytes.len() > offset {
                        pending.extend_from_slice(&bytes[offset..]);
                        offset = bytes.len();
                        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                            let line = pending.drain(..=newline).collect::<Vec<_>>();
                            if line.windows(MARKER.len()).any(|window| window == MARKER) {
                                let _ = gate.release(epoch);
                                return;
                            }
                        }
                        if pending.len() > 4096 {
                            let keep_from = pending.len() - MARKER.len();
                            pending.drain(..keep_from);
                        }
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }
        })
        .expect("failed to start serial milestone observer");
}

fn current_placeholder(config: &LinuxQemuConfig) -> String {
    config
        .guest
        .run_dir
        .join("usb.empty.medium")
        .to_string_lossy()
        .into_owned()
}
