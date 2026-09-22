# Linux and WSL2

The `cdj3k-emu-wsl` application is a Linux port of the original macOS
emulator. It keeps the native egui chassis and firmware-install workflow while
running a per-instance `qemu-system-aarch64` child with TCG acceleration.

The normal WSL2 target is Ubuntu 24.04 with WSLg. QEMU uses user-mode NAT by
default, PulseAudio is opt-in, and both LCDs are delivered through the existing
SHM transports: `main.shm` for the 1280×720 display and `jog.shm` for the
320×240 jog display. No privileged TAP setup or physical USB access is needed
for a basic launch.

Building the workspace requires Rust/Cargo 1.85 or newer, matching the
workspace `rust-version`. Check the selected tools with `cargo --version` and
`rustc --version` before building. If a system Cargo shadows an installed
rustup toolchain, select the latter first, for example with
`export PATH="$HOME/.cargo/bin:$PATH"`. The WSL preflight fails early if either
selected tool is older than 1.85. Firmware provisioning requires `cpio` on
`PATH`. Virtual-media image creation additionally requires either `mkfs.exfat`
or `mkfs.vfat` on `PATH`; `scripts/wsl/run.sh --virtual-media` warns if neither
is available. Mounting an existing image does not require a formatter; Create
Image does.

## First run

Build non-proprietary resources and the pinned upstream QEMU 10.2.2 binary:

```sh
scripts/wsl/build-qemu.sh
scripts/wsl/build-resources.sh
scripts/wsl/preflight.sh
```

The ARM64 resource build can also run independently of the invoking terminal.
This keeps a long emulated kernel compile alive across agent or shell sessions
and writes its complete output under the ignored `build/` directory:

```sh
scripts/wsl/build-resources.sh --background
# Check once after the build has had time to finish.
scripts/wsl/build-resources.sh --status
```

The start command prints the exact log path. A successful status exits 0, a
failed status exits 1, and a build still in progress exits 3.

The resource builder uses Docker Buildx with a `linux/arm64` builder (native
arm64 or host emulation). QEMU needs Meson, Ninja, a C toolchain, and the
dependencies selected by its pinned configure command. The scripts validate
these requirements during a clean build; cached outputs are not evidence of
a successful build from the current source.

The resource builder produces one canonical layout:

```text
build/wsl-resources/
├── Image
├── modules/{subucom_virt,virtio_snd,udev_usb1}.ko
├── tools/{ep122_shim.so,subucom_forwarder,subucom_live,cfgd}
└── patch/{patch-rootfs.sh,patch-rootfs.d/,dummy_drv.so,vanilla-modules/*.ko}
```

The layout is non-proprietary. Firmware updates, keys, eMMC images, and
extracted firmware data stay in the per-instance data directory.

The original R&D archive recorded an internal source snapshot as
`7c3db1e…`; that identifier is not present in upstream QEMU GitLab. The clean
workflow therefore pins the verified upstream `v10.2.2` commit
`f8ed81651e61d9c2166df6121ce2af0f44f06b3e` and applies the three
repository-owned patches in `qemu/patches/wsl/` locally. The original QEMU
build uses its explicit top-level patch manifest and never reads that directory.

Then launch with `scripts/wsl/run.sh`. If the instance has no `Image`, patched
initramfs, or eMMC image, the application opens **Install Firmware**. Select
the local `.UPD` and the path to a user-owned keyfile; generated files remain
in the normal per-instance XDG data directory. The key is read for provisioning and is not copied into
the resource or runtime directories.

Useful options include `--instance`, `--kernel`, `--initramfs`, `--qemu`,
`--resources`, `--audio`, `--no-audio`, `--audio-device`, `--no-network`,
`--virtual-media`, and `--no-emmc`.

## Runtime boundary

`GuestRuntimeConfig` describes the guest topology. `LinuxQemuConfig` adds only
Linux host choices, and `QemuInstance::spawn_linux` owns the external child,
QMP connection, eMMC lock, restart, and bounded shutdown. The macOS app keeps
its existing HVF and QEMU FFI path.

This port does not claim physical USB, bridged DJ Link, Linux packaging, or
firmware qualification. Those are separate follow-up work.

## Verification status

The current clean-branch results and remaining gates are recorded in
[WSL review validation](wsl-validation.md).

Source-level validation and host-side WSL acceptance are separate gates. The
following require the documented host dependencies and have not been asserted
by this source tree alone:

- QEMU 10.2.2 build and non-proprietary resource staging
- WSL preflight and WSLg application startup
- first-run wizard path and per-instance state provisioning

The frozen review source commit has completed three firmware-backed boots on
WSL2, including the EP122 application, main and jog displays, bidirectional
control transport, graceful shutdown, in-process restart, and full application
relaunch. Exact results and limitations are in the validation record. Live
guest audio, NAT, physical USB, and virtual media remain outside that result.

No CDJ-3000 `.UPD` file or decryption key is included in the repository.
Proprietary firmware and key material must remain user-owned and outside it.
