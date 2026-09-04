use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use cdj3k_emu_platform::menu_state::{self, APP_SHUTDOWN};
use cdj3k_emu_runtime::register_worker_thread;
use cdj3k_emu_runtime::{LinuxQemuConfig, QemuInstance};
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

    while !APP_SHUTDOWN.load(Ordering::Relaxed) {
        let (
            audio_changed,
            audio_enabled,
            audio_device,
            service_mode,
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
                std::mem::take(&mut state.usb_virtual_mount_req),
                std::mem::take(&mut state.usb_eject_req),
                state.usb_virtual_img.clone(),
            )
        };
        config.guest.service_mode = service_mode;

        if audio_changed {
            config.audio = audio_enabled;
            config.audio_device = audio_device;
            control_gate.invalidate();
            if let Some(mut old) = instance.take() {
                old.stop();
            }
            instance = start_instance(&config, true, Arc::clone(&control_gate));
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
            if media_mount {
                if let Some(path) = media_path.as_ref().filter(|path| path.exists()) {
                    match current.qmp().blockdev_change_medium(
                        "usb0",
                        &path.to_string_lossy(),
                        "raw",
                    ) {
                        Ok(_) => menu_state::lock().usb_virtual_mounted = true,
                        Err(error) => {
                            eprintln!("cdj3k-emu-wsl: virtual media mount failed: {error:?}")
                        }
                    }
                }
            }
            if media_eject {
                let placeholder = current_placeholder(&config);
                match current
                    .qmp()
                    .blockdev_change_medium("usb0", &placeholder, "raw")
                {
                    Ok(_) => menu_state::lock().usb_virtual_mounted = false,
                    Err(error) => eprintln!("cdj3k-emu-wsl: virtual media eject failed: {error:?}"),
                }
            }
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
            }
            Some("restart") => {
                control_gate.invalidate();
                if let Some(mut old) = instance.take() {
                    old.stop();
                }
                instance = start_instance(&config, true, Arc::clone(&control_gate));
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
        menu_state::lock().qemu_running = running;
        thread::sleep(Duration::from_millis(100));
    }

    if let Some(mut instance) = instance {
        instance.stop();
    }
    menu_state::lock().qemu_running = false;
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
