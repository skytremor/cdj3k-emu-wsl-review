//! Linux/WSL2 entry point.  This binary composes the existing native chassis
//! UI with an external QEMU process; it does not alter the macOS application.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cdj3k-emu-wsl is supported on Linux hosts only");
}

#[cfg(target_os = "linux")]
mod runtime;

#[cfg(target_os = "linux")]
mod linux {
    use clap::Parser;
    use eframe::egui::{FontData, FontDefinitions, FontFamily, FontTweak};
    use std::path::PathBuf;
    use std::sync::Arc;

    use cdj3k_emu_streams::ControlConnectionGate;

    #[derive(Debug, Parser)]
    #[command(name = "cdj3k-emu-wsl", about = "Launch the CDJ-3000 UI on Linux/WSL2")]
    pub struct Args {
        #[arg(long, default_value_t = 1)]
        pub instance: u32,
        #[arg(long)]
        pub kernel: Option<PathBuf>,
        #[arg(long)]
        pub initramfs: Option<PathBuf>,
        #[arg(long)]
        pub qemu: Option<PathBuf>,
        #[arg(long)]
        pub qemu_img: Option<PathBuf>,
        #[arg(long)]
        pub resources: Option<PathBuf>,
        #[arg(long)]
        pub audio: bool,
        #[arg(long)]
        pub no_audio: bool,
        #[arg(long)]
        pub audio_device: Option<String>,
        #[arg(long)]
        pub no_network: bool,
        #[arg(long)]
        pub virtual_media: bool,
        #[arg(long)]
        pub no_emmc: bool,
        /// Open the existing firmware wizard instead of starting QEMU.
        #[arg(long)]
        pub install_firmware: bool,
        /// Retained for script compatibility; currently enables no profiler.
        #[arg(long)]
        pub profile: bool,
    }

    fn configure_fonts(ctx: &eframe::egui::Context) {
        const REGULAR: &[u8] = include_bytes!("../../cdj3k-emu/assets/nimbus-sans-l.regular.otf");
        const BOLD: &[u8] = include_bytes!("../../cdj3k-emu/assets/nimbus-sans-l.bold.otf");
        const CONDENSED: &[u8] =
            include_bytes!("../../cdj3k-emu/assets/nimbus-sans-t.regular.condensed.otf");
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "nimbus-sans".into(),
            FontData::from_static(REGULAR).tweak(FontTweak {
                y_offset_factor: 0.25,
                ..Default::default()
            }),
        );
        fonts.font_data.insert(
            "nimbus-sans-bold".into(),
            FontData::from_static(BOLD).tweak(FontTweak {
                y_offset_factor: 0.25,
                ..Default::default()
            }),
        );
        fonts.font_data.insert(
            "nimbus-sans-condensed".into(),
            FontData::from_static(CONDENSED).tweak(FontTweak {
                y_offset_factor: 0.15,
                ..Default::default()
            }),
        );
        if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
            family.insert(0, "nimbus-sans".into());
        }
        for name in ["nimbus-sans", "nimbus-sans-bold", "nimbus-sans-condensed"] {
            fonts
                .families
                .insert(FontFamily::Name(name.into()), vec![name.into()]);
        }
        ctx.set_fonts(fonts);
    }

    fn main_impl() -> eframe::Result {
        let args = Args::parse();
        let instance = args.instance.max(1);
        cdj3k_emu_platform::menu_state::lock().current_instance_id = instance;

        let instance_dir = cdj3k_emu_storage::default_path(instance)
            .parent()
            .expect("eMMC path has a parent")
            .to_path_buf();
        let socket_dir = cdj3k_emu_platform::runtime_paths::instance_dir(instance);
        let kernel = args.kernel.unwrap_or_else(|| instance_dir.join("Image"));
        let initramfs = args
            .initramfs
            .unwrap_or_else(|| instance_dir.join("initramfs-patched.cpio.gz"));
        let emmc = (!args.no_emmc).then(|| cdj3k_emu_storage::default_path(instance));
        let settings = cdj3k_emu_storage::InstanceSettings::load_or_init(instance);
        {
            let mut state = cdj3k_emu_platform::menu_state::lock();
            state.audio_enabled = settings.audio_enabled;
            state.audio_device_uid = settings.audio_device_uid.clone();
            if let Some(path) = settings
                .usb_virtual_path
                .as_ref()
                .filter(|path| path.exists())
            {
                state.usb_virtual_img = Some(path.clone());
            }
        }
        let qemu = args
            .qemu
            .unwrap_or_else(|| PathBuf::from("qemu-system-aarch64"));
        let run_dir = socket_dir.clone();
        let guest = cdj3k_emu_runtime::GuestRuntimeConfig {
            instance_id: instance,
            kernel,
            initramfs,
            emmc_img: emmc,
            run_dir,
            qmp_port: 4445 + instance as u16,
            ssh_port: 2222 + instance as u16,
            mac: settings.mac,
            service_mode: false,
        };
        let config = cdj3k_emu_runtime::LinuxQemuConfig {
            guest,
            qemu,
            audio: (args.audio || settings.audio_enabled) && !args.no_audio,
            audio_device: args.audio_device.or(settings.audio_device_uid),
            network: !args.no_network,
            virtual_media: args.virtual_media,
            serial_log: None,
            qmp_socket: None,
        };
        let start_qemu = !args.install_firmware
            && config.guest.kernel.exists()
            && config.guest.initramfs.exists()
            && (config.guest.emmc_img.is_none()
                || config.guest.emmc_img.as_ref().is_some_and(|p| p.exists()));
        if !start_qemu {
            cdj3k_emu_platform::menu_state::lock().firmware_wizard_requested = true;
        }

        let options = cdj3k_emu_platform::desktop::native_options(instance);
        let control_gate = Arc::new(ControlConnectionGate::new());
        eframe::run_native(
            &format!(
                "{} - {}",
                cdj3k_emu_platform::app_meta::APP_DISPLAY_NAME,
                instance
            ),
            options,
            Box::new(move |cc| {
                configure_fonts(&cc.egui_ctx);
                let app = cdj3k_emu_ui::app::CdjApp::new_with_options(
                    socket_dir.to_string_lossy().into_owned(),
                    cc.egui_ctx.clone(),
                    args.profile,
                    cdj3k_emu_ui::app::CdjAppOptions {
                        control_gate: Some(Arc::clone(&control_gate)),
                        firmware_resources: args.resources.clone(),
                        qemu_img: args.qemu_img.clone(),
                    },
                );
                crate::runtime::spawn(config, start_qemu, Arc::clone(&control_gate));
                Ok(Box::new(app))
            }),
        )
    }

    pub fn run() -> eframe::Result {
        main_impl()
    }
}

#[cfg(target_os = "linux")]
fn main() -> eframe::Result {
    linux::run()
}
