# Linux and WSL2

The `cdj3k-emu-wsl` application is a Linux port of the original macOS
emulator. It keeps the native egui chassis and firmware-install workflow while
running a per-instance `qemu-system-aarch64` child with TCG acceleration.

The normal WSL2 target is Ubuntu 24.04 with WSLg. QEMU uses user-mode NAT by
default, PulseAudio is opt-in, and both LCDs are delivered through the existing
SHM transports: `main.shm` for the 1280×720 display and `jog.shm` for the
320×240 jog display. No privileged TAP setup or physical USB access is needed
for a basic launch.

## First run

Build non-proprietary resources and the pinned upstream QEMU 10.2.2 binary:

```sh
scripts/wsl/build-qemu.sh
scripts/wsl/build-resources.sh
scripts/wsl/preflight.sh
```

The resource builder produces one canonical layout:

```text
build/wsl-resources/
├── Image
├── modules/{subucom_virt,virtio_snd,udev_usb1}.ko
├── tools/{ep122_shim.so,subucom_forwarder,subucom_live,cfgd}
└── patch/{patch-rootfs.sh,dummy_drv.so,vanilla-modules/*.ko}
```

The layout is non-proprietary. Firmware updates, keys, eMMC images, and
extracted firmware data stay in the per-instance data directory.

The original R&D archive recorded an internal source snapshot as
`7c3db1e…`; that identifier is not present in upstream QEMU GitLab. The clean
workflow therefore pins the verified upstream `v10.2.2` commit
`f8ed81651e61d9c2166df6121ce2af0f44f06b3e` and applies the three
repository-owned SHM-display patches locally.

Then launch with `scripts/wsl/run.sh`. If the instance has no `Image`, patched
initramfs, or eMMC image, the application opens **Install Firmware**. Select
the local `.UPD` and key; generated files remain in the normal per-instance
XDG data directory. The key is read for provisioning and is not copied into
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

Source-level validation and host-side WSL acceptance are separate gates. The
following require the documented host dependencies and have not been asserted
by this source tree alone:

- QEMU 10.2.2 build and non-proprietary resource staging
- WSL preflight and WSLg application startup
- first-run wizard path and per-instance state provisioning

Firmware-dependent verification remains user-owned:

- completion of firmware provisioning
- genuine EP122 boot, main display, jog display, controls, and restart
- live guest audio, NAT, and virtual media

No authorized local CDJ-3000 `.UPD` file or decryption key is included in the
repository or supplied by the clean-port validation. Proprietary firmware and
key material must remain user-owned and outside the repository.
