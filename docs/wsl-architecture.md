# Linux/WSL architecture

This project runs real CDJ-3000 firmware inside an AArch64 Linux guest and presents it through a native host application. The original host is Apple Silicon macOS. The Linux host, used primarily through x86_64 WSL2, adapts that design rather than creating a second emulator.

Most of the UI, firmware handling, storage model, control protocol, and guest hardware emulation is shared. The host-specific code chooses how QEMU runs, how its lifecycle is managed, and which audio, network, menu, and transport policies apply.

## System at a glance

```text
                              CDJ firmware (EP122)
                                       │
                             AArch64 Linux 6.6 guest
          ┌────────────────────┬────────┼───────────────┬────────────────┐
          │                    │        │               │                │
      main display         jog display controls      config        block storage
       virtio-gpu       DSI-2 + shim  subucom_virt    cfgd          virtio-blk
          │                    │        │               │                │
     QEMU shm-display      ivshmem  cdj3k.ctrl     cdj3k.cfg       eMMC / media
          │                    │        │               │                │
       main.shm             jog.shm  ctrl.sock       cfg.sock            │
          └────────────────────┴────────┴───────┬───────┘                │
                                                │                        │
                                      Shared Rust application ───────────┘
                                  UI · streams · firmware · storage
                                                │
                              ┌─────────────────┴─────────────────┐
                              │                                   │
                         macOS backend                       Linux/WSL backend
                    patched QEMU via FFI                    QEMU child process
                         HVF acceleration                   TCG emulation
                         TCP QMP                            Unix-socket QMP
                         CoreAudio bypass                   PulseAudio → WSLg
                         vmnet/TAP/NAT                      user-mode NAT
                         native menu                        egui operator bar
```

The guest sees essentially the same emulated machine on either host. The differences below the shared Rust layer are deliberate host policies, not separate guest designs.

## Layers and ownership

### Host applications

`app/cdj3k-emu` is the macOS application. It starts the patched macOS QEMU through an FFI-backed dylib, uses HVF by default, and retains the native menu and established macOS lifecycle.

`app/cdj3k-emu-wsl` is the Linux entry point. It builds the same `CdjApp` UI with Linux-specific `CdjAppOptions`, constructs a `LinuxQemuConfig`, and starts an external `qemu-system-aarch64` child. Its `runtime.rs` worker owns start, stop, restart, audio reconfiguration, optional virtual media, and Linux diagnostics.

The Linux application is therefore a different composition of shared pieces, not a separate UI implementation.

### Shared Rust crates

| Crate                | Owns                                                                                                                        |
| -------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `cdj3k-emu-platform` | Runtime paths, shared operator state, native desktop integration, menus, haptics, and host audio-device discovery           |
| `cdj3k-emu-runtime`  | Guest and QEMU configuration, QMP, process lifecycle, shutdown, instance ownership, and runtime storage/network integration |
| `cdj3k-emu-streams`  | Main and jog display readers, the bidirectional control socket, repaint coordination, and optional control-readiness gating |
| `cdj3k-emu-ui`       | Chassis/deck UI, OpenGL textures, touch and jog translation, operator controls, firmware wizard, and secondary viewports    |
| `cdj3k-emu-storage`  | Per-instance eMMC images and persistent settings                                                                            |
| `cdj3k-emu-firmware` | Firmware decryption and extraction, initramfs discovery, patching, and resource validation                                  |
| `cdj3k-emu-subucom`  | MISO/MOSI frame definitions, field access, and CRC handling                                                                 |

### Repository map

```text
app/
  cdj3k-emu/             macOS launcher and runtime worker
  cdj3k-emu-wsl/         Linux/WSL launcher and runtime worker

crates/                  shared host-side libraries
guest/                   guest modules, services, kernel patches, and EP122 shim
qemu/patches/            macOS QEMU patch series
qemu/patches/wsl/        Linux/WSL QEMU patch series
scripts/wsl/             WSL build, preflight, test, and run workflow
docker/                  reproducible AArch64 guest-resource build
initramfs-patch/         guest root-filesystem provisioning steps
```

## The host boundary

| Concern        | macOS backend                       | Linux/WSL backend                              |
| -------------- | ----------------------------------- | ---------------------------------------------- |
| CPU execution  | HVF, normally with host CPU         | Multi-threaded TCG with `cortex-a72`           |
| QEMU ownership | Patched QEMU dylib through FFI      | Directly owned child process                   |
| QMP            | TCP on a per-instance port          | Launch-unique Unix socket                      |
| Main display   | Shared-memory display               | Shared-memory display                          |
| Jog display    | ivshmem shared memory               | ivshmem shared memory                          |
| Audio          | Patched virtio-snd/CoreAudio bypass | Stock QEMU virtio-sound to PulseAudio          |
| Network        | NAT, vmnet bridging, or TAP         | User-mode NAT when enabled                     |
| Operator UI    | Native application menu             | In-window egui operator bar                    |
| Cleanup        | Existing macOS lifecycle            | Linux-owned child and transient-file lifecycle |
| Diagnostics    | Optional traditional serial log     | Launch-unique serial logs are retained         |

“Implemented” and “validated” are intentionally different claims. A backend may be present in QEMU arguments and still lack firmware-backed qualification.

## Two QEMU build paths

The hosts need different implementations of the same conceptual facilities, so their patch series and build scripts remain separate.

The macOS build in `qemu/build.sh` pins QEMU to `ee7eb612be8f8886d48c1d0c1f1c65e495138f83` and applies an explicit 12-patch manifest from `qemu/patches/`. That series includes ivshmem and shared-display support, the custom CoreAudio/virtio-snd bypass, and macOS scheduling and HVF changes. It also builds the dylib consumed by the Rust runtime.

The WSL build in `scripts/wsl/build-qemu.sh` pins upstream QEMU 10.2.2 at `f8ed81651e61d9c2166df6121ce2af0f44f06b3e`. It applies only three shared-display patches from `qemu/patches/wsl/`, then builds `aarch64-softmmu` with TCG, SLIRP, and PulseAudio. The Linux runtime launches the installed executable directly.

Keeping explicit manifests prevents a host build from accidentally consuming an incompatible variant placed under the other host’s directory.

## Guest emulation layer

Both hosts boot the same Linux 6.6-based AArch64 guest and stage the same guest-side hardware substitutes:

- `virtio-gpu` supplies the main DRM display. A guest kernel patch exposes a second synthetic DSI-2 connector and its required 1280×240 mode for the jog display.
- `ep122_shim.so` is loaded into EP122 with `LD_PRELOAD`. It adapts hardware-facing calls, enforces the synthetic jog topology, reads the jog dumb buffer, and publishes its visible 320×240 image through ivshmem.
- `subucom_virt.ko` replaces the hardware SPI/sub-controller interface expected by EP122. It carries host input toward the firmware and LED state back toward the host.
- `subucom_forwarder` bridges that kernel interface to the `cdj3k.ctrl` virtio-serial port.
- `virtio_snd.ko` provides the guest ALSA/virtio-sound side of the audio path.
- `udev_usb1.ko` recreates `/proc/udev_usb1`, the USB event channel expected by the firmware.
- `cfgd` exposes a small line-based control plane over `cdj3k.cfg` for virtual-media actions and audio parameters/telemetry.

This is the shared hardware-emulation boundary. A kernel, module, initramfs, or EP122-shim change can affect both host applications even when no macOS host code changes. Treat guest changes as cross-host until hardware testing proves otherwise.

## Display paths

### Main LCD

```text
guest DRM / virtio-gpu
→ QEMU shared-memory display backend
→ main.shm
→ MainLcdStream
→ OpenGL texture
→ chassis UI or separate viewport
```

`main.shm` contains a small header, dirty rectangle, and framebuffer payload. `MainLcdStream` watches its magic and generation fields, accumulates dirty regions when the UI is busy, and remaps after QEMU replaces the file.

The reader supports two publication contracts:

- `LegacyPublishedGeneration` is the backward-compatible default. Every changed generation is a published frame, including odd generations. The notification keeps an `Arc<Mmap>` and the source surface stride, preserving the existing mapped, zero-copy-style upload path used by the macOS writer.
- `SequencedStableSnapshot` treats the generation as a seqlock. Odd means QEMU is writing; a stable, nonzero even value is publishable. The reader checks the generation before and after copying the dirty region into owned, tightly packed rows. Linux selects this mode explicitly.

These are wire-protocol choices. A future backend must choose the contract its QEMU producer actually implements rather than redefining the shared reader’s default.

### Jog LCD

```text
guest DRM synthetic DSI-2 output
→ EP122 dumb framebuffer
→ ep122_shim crop and publication
→ ivshmem-backed jog.shm
→ JogLcdStream
→ egui/OpenGL texture
```

The guest kernel and shim share `guest/kernel-patches/cdj3k_jog_mode.h`, which fixes the mode and connector topology that EP122 expects. The firmware draws a stretched 1280×240 buffer; the shim extracts the wrapped, visible 320×240 panel image.

Publication uses a sequence counter: odd while pixels are being written, even when stable. A dedicated shim worker samples the active framebuffer at 60 Hz, while ioctl-triggered publication can reduce latency. The host polls at roughly 60 Hz and accepts a frame only when the sequence is unchanged across the copy.

## Controls and configuration

The control socket carries the virtual sub-controller protocol in both directions:

```text
buttons, touch, jog
→ host MISO frame → ctrl.sock → subucom_forwarder
→ /dev/subucom_ctrl → subucom_virt → EP122

EP122 LED/hardware state
→ subucom_virt → /dev/subucom_ctrl → subucom_forwarder
→ ctrl.sock → CtrlStream → host LEDs and state
```

`cdj3k-emu-subucom` owns the 64-byte frame representation and CRC rules. `CtrlStream` connects lazily, retries after disconnects, retains the latest MOSI frame, and wakes the UI only when visible state changes.

`cfg.sock` is separate. `CfgClient` and guest `cfgd` exchange newline-delimited commands for virtual-media attach/detach, whitelisted audio settings, and latency telemetry. It is a control plane, not part of the high-rate input/LED stream.

### Optional readiness gate

The shared stream constructors are ungated by default. Linux supplies a `ControlConnectionGate` so `CtrlStream` does not connect until the current launch has produced:

1. a nonzero main-display generation at 1280×720; and
2. two distinct nonzero even jog publications accepted by the jog reader.

Every launch receives a new `LaunchEpoch`. Display readers capture that epoch when they map SHM, and observations from an old mapping cannot satisfy a newer launch. Stop, crash, audio-triggered restart, and manual restart invalidate the current epoch.

The serial message `Application Started` is tracked against the same epoch and shown as an operator milestone. It is **not** a prerequisite for the gate: display readiness is independent, and the control connection may become ready before the serial marker.

## Linux launch and lifecycle

The normal path is:

```text
scripts/wsl/run.sh
→ dependency/resource preflight
→ cargo run -p cdj3k-emu-wsl
→ shared CdjApp + LinuxQemuConfig
→ QemuInstance::spawn_linux()
→ external QEMU child and Unix QMP negotiation
→ guest boot and display readiness
→ control connection
```

`run.sh` supplies the locally built QEMU and resource tree, selects software GL defaults suitable for WSLg, and forwards application options. Missing per-instance firmware is allowed: the application opens the shared firmware wizard instead of starting QEMU.

For each launch, `spawn_linux()`:

- takes an advisory lock on the runtime instance directory before touching launch-owned endpoints;
- removes only stale QMP sockets;
- creates fresh main/jog SHM files and, when enabled, a virtual-media placeholder;
- takes a second nonblocking lock on the persistent eMMC image;
- creates unique QMP and serial-log paths;
- starts QEMU with a parent-death signal; and
- waits up to 15 seconds for Unix-socket QMP negotiation.

Shutdown first asks the UI to send EP122’s power-off control stimulus, then attempts ACPI powerdown and QMP quit. A bounded SIGKILL fallback remains if QEMU does not exit. Linux cleanup removes launch-owned sockets and SHM files, but preserves the runtime directory, lock file, and serial logs for diagnosis. A restart drops the old instance—and therefore its locks—before beginning a new launch epoch.

macOS intentionally retains its established FFI, TCP-QMP, stale-instance, and cleanup policies.

## Firmware provisioning

Firmware is installed locally through the shared wizard:

```text
user-owned .UPD + user-owned keyfile
→ decrypt update to a temporary ISO
→ read firmware metadata and extract kernels
→ extract and patch the initramfs
→ stage guest modules, tools, and services
→ create per-instance sparse eMMC qcow2
→ launch
```

The wizard writes `Image`, `initramfs-patched.cpio.gz`, `emmc.qcow2`, and settings under the instance’s application-data directory. On Linux that base is `$XDG_DATA_HOME/<bundle-id>` or `~/.local/share/<bundle-id>`; macOS uses `~/Library/Application Support/<bundle-id>`.

The `.UPD` and key remain user-owned inputs. The key is read from its selected path and is not copied into the resource tree, runtime directory, bundle, or repository. Temporary decrypted material is removed after provisioning.

The firmware crate accepts two resource layouts containing the same logical inputs:

- `MergedDispatcher` is the packaged macOS form, where the numbered rootfs steps are merged into the generated `patch-rootfs.sh`.
- `DirectoryDispatcher` is the Linux/WSL resource tree, where `patch-rootfs.sh` dispatches to a validated `patch-rootfs.d/` directory.

Supporting both layouts keeps provisioning shared without requiring the two distribution formats to be identical.

## Persistent and transient state

The storage model is per instance. Its main persistent artifact is a sparse 29.1 GB eMMC qcow2 with a CDJ-compatible GPT, U-Boot environment, settings partition, and user-data partition. Plain-text settings persist the instance MAC, audio choices, ALC and haptic preferences, network selection, and virtual/physical media choices. Unknown settings keys survive rewrites for forward compatibility.

Keep the three state classes distinct:

| State                    | Examples                                                                      | Lifetime                        |
| ------------------------ | ----------------------------------------------------------------------------- | ------------------------------- |
| Persistent instance data | `Image`, patched initramfs, `emmc.qcow2`, `settings.txt`                      | Across launches and upgrades    |
| Transient runtime data   | `main.shm`, `jog.shm`, `ctrl.sock`, `cfg.sock`, QMP socket, media placeholder | One QEMU launch                 |
| Diagnostics              | Launch-unique Linux serial logs                                               | Preserved across Linux restarts |

QEMU drive-level locking is disabled because the host runtime owns exclusivity. The eMMC file lock prevents two processes from writing the same image, while the Linux instance-directory lock prevents two processes from owning the same sockets and SHM paths.

Virtual media exists on Linux behind the explicit `--virtual-media` option. Its basic create, attach, and eject plumbing is implemented through QMP and `cfgd`, but the current device ordering needs separate review before this is treated as qualified firmware behavior. Physical removable-disk integration remains macOS-specific.

## Audio and networking

### Audio

The shared guest side begins with EP122/JUCE, ALSA, and `virtio_snd.ko`. After that the host paths diverge.

On macOS, patched QEMU moves virtio-sound data through a dedicated writer, conversion/resampling path, and lock-free CoreAudio bypass. This is a specialized low-latency design; see [Audio stack](audio-stack.md).

Linux/WSL currently uses the stock QEMU backend:

```text
guest virtio_snd
→ QEMU virtio-sound-device
→ PulseAudio
→ WSLg
→ Windows audio
```

The launcher can enumerate/select PulseAudio outputs and restart QEMU when audio configuration changes. The path is implemented, but firmware-backed validation did not cover live audio. No claim is made for sustained playback, XRUN behavior, latency, or parity with the macOS bypass.

### Networking

Linux/WSL adds QEMU user-mode networking when networking is enabled. It supplies guest DHCP/DNS/outbound access and forwards a per-instance host port to guest SSH.

This is NAT, not a Layer-2 bridge. Pro DJ Link discovery and timing depend on LAN broadcast behavior, so WSL NAT is not equivalent to the macOS vmnet or TAP paths and is not a validated route to physical players.

The macOS runtime can select user-mode NAT, a `socket_vmnet` bridge to a host interface, or a TAP bridge. Those lifecycle and privilege details remain macOS backend concerns; see [Network stack](network.md).

## Explicit platform policy

Shared code keeps backward-compatible behavior by default. A host launcher opts into alternatives deliberately:

- `MainDisplayMode` selects the framebuffer publication contract.
- `BootShadeMode` preserves the legacy macOS frame-threshold shade or selects Linux live-status behavior.
- `ControlConnectionGate` is optional; Linux supplies it, legacy constructors do not.
- Firmware resource profiles accept either the macOS bundle or Linux directory layout without conflating them.
- `GuestRuntimeConfig` describes shared guest topology, while `LinuxQemuConfig` adds Linux executable and backend choices beside the existing macOS `QemuConfig`.

This is the preferred extension pattern. Avoid scattering broad `if macOS`/`if Linux` branches through shared behavior, and do not silently change a shared protocol to match one producer.

### Adding another host

A native Linux or Windows backend will generally need to provide:

- a launcher and host-specific QEMU configuration;
- process, QMP, shutdown, and cleanup ownership;
- suitable audio and network backends; and
- host-native operator or device-selection integration where needed.

It should reuse the shared UI, firmware provisioning, storage model, subucom protocol, display abstractions, and guest emulation where their contracts fit. These are intended boundaries, not a guarantee that every current implementation is already portable without work.

## Validation snapshot

The following qualification snapshot applies to the frozen WSL review source at `2de7994d7c3b5b93a97e0b0e6de7a9719e3d4fd4`. Architecture and implementation may evolve independently of this snapshot. See [WSL review validation](wsl-validation.md) for the authoritative evidence, exact environment, revision lineage, and any newer results.

| Capability                           | Architecture             | Review qualification                                  |
| ------------------------------------ | ------------------------ | ----------------------------------------------------- |
| AArch64 firmware boot under TCG      | Implemented              | Firmware-backed                                       |
| Main LCD                             | Implemented              | Firmware-backed at 1280×720                           |
| Jog LCD                              | Implemented              | Firmware-backed at 320×240                            |
| Bidirectional controls               | Implemented              | Firmware-backed in both directions                    |
| In-process restart and full relaunch | Implemented              | Firmware-backed                                       |
| Graceful host shutdown               | Implemented              | Partial; bounded SIGKILL fallback remains             |
| Audio pipeline                       | Implemented              | Not firmware-qualified                                |
| User-mode NAT                        | Implemented              | Excluded from the firmware-backed review              |
| Physical Pro DJ Link / Layer 2       | No qualified WSL backend | Not validated                                         |
| Virtual media                        | Partial                  | Outside core validation; device ordering needs review |
| Native Linux packaging               | Not implemented          | Not validated                                         |

The macOS-specific source contracts and original QEMU patches were reviewed, but the shared guest/runtime changes have not been run on Mac hardware. Source compatibility is not runtime qualification.

## Architectural invariants

- Proprietary firmware and key material are never repository resources.
- The macOS and WSL QEMU manifests remain explicit and separate.
- Shared constructors preserve legacy behavior unless a host opts into another policy.
- Guest changes are considered cross-host until validated otherwise.
- Persistent data and transient runtime endpoints remain per instance.
- Old mappings, logs, or observers cannot satisfy readiness for a newer launch epoch.
- A platform-specific backend must not silently redefine a shared wire protocol.
- Validation claims describe tested evidence, not merely reachable code paths.

## Limitations and non-goals

- x86_64 WSL runs the AArch64 guest under TCG. This design does not claim cross-architecture KVM acceleration.
- The Linux/WSL work is not yet a generic accelerated AArch64 appliance or a packaged native-Linux product.
- Linux audio quality and physical Layer-2 Pro DJ Link remain unqualified.
- Optional virtual media is not part of the core validated path; physical USB passthrough is not claimed.
- Shared guest changes still require Mac-hardware validation.
- These boundaries guide future integration, but do not imply that every change should be merged wholesale into a newer upstream revision.

## Where to start reading

For the shortest path from overview to implementation:

1. This document.
2. [Linux and WSL2](linux-wsl.md) for the build and first-run workflow.
3. `app/cdj3k-emu-wsl/src/main.rs` for Linux composition and policy selection.
4. `app/cdj3k-emu-wsl/src/runtime.rs` for launch, restart, readiness epochs, and operator actions.
5. `crates/cdj3k-emu-runtime/src/config.rs` and `instance.rs` for QEMU topology and ownership.
6. `crates/cdj3k-emu-streams/src/{main_stream,jog_stream,ctrl_stream,control_gate}.rs` for transport contracts.
7. `guest/ep122_shim/`, `guest/modules/`, and `guest/subucom/` for the guest hardware boundary.
8. `qemu/patches/wsl/` and `scripts/wsl/build-qemu.sh` for the Linux display backend.

Specialist references:

- [Host/guest stream transports](stream-transports.md)
- [Subucom protocol](subucom.md)
- [Storage](storage.md)
- [Audio stack](audio-stack.md)
- [Network stack](network.md)
- [WSL validation](wsl-validation.md)
- [macOS compatibility review](osx-review-results.md)
