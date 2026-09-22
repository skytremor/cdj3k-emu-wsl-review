# macOS Compatibility Audit Results

## 1. Executive verdict

The WSL branch at `26d0f453aa68e308d1c19ad08de9ce262def1aa2` contained four definite host-side macOS regressions:

1. the shared main-display reader required WSL's even-generation seqlock protocol and no longer accepted the original macOS QEMU writer's monotonically incremented odd and even generations;
2. shared boot-shade and native-menu behavior no longer preserved the upstream macOS launch and restart lifecycle;
3. shared instance locking and cleanup preserved the runtime directory on macOS, which changed stale recovery and could leave stopped instances shown as running;
4. firmware resource validation required the WSL directory-dispatcher layout and rejected the merged dispatcher produced by the macOS bundle.

All four were fixed in source commit `2de7994d7c3b5b93a97e0b0e6de7a9719e3d4fd4` (`2de7994`) by restoring upstream-compatible defaults and placing WSL behavior behind explicit modes or Linux-only branches. Focused contract tests cover the source-level behavior. The current WSL QEMU was rebuilt and the final source completed one end-to-end smoke with two successful launches: QMP and control sockets connected, the main display published a stable even generation at 1280×720 with nonblack pixels, the jog display worked, transient cleanup completed, and relaunch succeeded. A normal window close exits the WSL host application cleanly; QEMU still requires the existing eight-second SIGKILL fallback after its graceful-shutdown window.

No macOS runtime test was performed. Shared guest-kernel and EP122-shim changes still execute inside the guest on both hosts. Therefore the result is source-level contract restoration, not a claim of macOS runtime compatibility.

> Darwin-specific runtime implementations and the contents of the original 12 QEMU patches remain unchanged; shared runtime/UI/build code around them has changed and is the subject of this audit.

## 2. Validation boundaries and results

| Check | Result |
| --- | --- |
| `git diff --check` | PASS |
| modern-toolchain `cargo metadata --no-deps --locked` | PASS |
| `cargo fmt --all --check` | PASS |
| `cargo test --workspace --locked`, normal parallel mode | PASS |
| serial workspace test diagnostic | PASS |
| `scripts/wsl/test.sh` | PASS |
| original QEMU manifest name and order | PASS: exactly patches `01` through `12` |
| WSL QEMU manifest name and order | PASS: exactly the three WSL patches |
| original patches applied independently to pinned macOS QEMU revision | PASS |
| WSL patches applied independently to pinned WSL QEMU revision | PASS |
| original 12 patch SHA-256 comparison | PASS: contents unchanged |
| WSL runtime smoke on final source | PASS: one end-to-end smoke with launch and successful relaunch after rebuilding current QEMU |
| WSL smoke coverage | QMP/control, stable even main generation, 1280×720 nonblack main frame, jog, cleanup, relaunch |
| WSL normal window close | Host exits cleanly; QEMU uses the existing 8 s SIGKILL fallback |
| macOS Rust type-check | NOT AVAILABLE |
| macOS runtime | NOT TESTED — REQUIRES MAC HARDWARE |

No OpenGL call was claimed as tested without a GL context. The display tests instead verify protocol selection, mapped versus owned payloads, row packing, accumulation, and frame accounting deterministically.

## 3. Complete mac-visible diff classification

Categories are:

- **A** — Linux-only;
- **B** — shared but backward-compatible;
- **C** — shared behavior intentionally changed;
- **D** — WSL behavior leaked into macOS in the frozen source and was a regression candidate; all D items below are resolved at `2de7994`;
- **E** — build metadata or documentation only.

For every B/C/D row, the “macOS surface” column states whether macOS compiles and executes the file, the “contract” column records upstream and current behavior, and the last two columns state observable impact and whether Linux behavior was isolated.

| # | File | Class | macOS surface | Upstream → final source behavior | Observable difference on macOS? | Isolation or disposition |
| ---: | --- | :---: | --- | --- | --- | --- |
| 1 | `.gitignore` | E | Not compiled or executed | Ignore-list maintenance only | No runtime effect | None needed |
| 2 | `Cargo.lock` | E | Used by Cargo, not executed | Dependency lock added | Build inputs are pinned; no application semantics | None needed |
| 3 | `Cargo.toml` | E | Parsed by Cargo | Workspace includes the Linux application | No macOS runtime path change | Linux binary is a separate workspace member |
| 4 | `README.md` | E | Not compiled or executed | Documentation extended for WSL | No | None needed |
| 5 | `app/cdj3k-emu-wsl/Cargo.toml` | A | No / no | No Linux application → separate WSL package | No | Separate package |
| 6 | `app/cdj3k-emu-wsl/src/main.rs` | A | No / no | No Linux launcher → WSL launcher with explicit sequenced-display and live-status shade modes | No | Linux-only crate and explicit opt-ins |
| 7 | `app/cdj3k-emu-wsl/src/runtime.rs` | A | No / no | No Linux runtime worker → Linux-owned worker | No | Linux-only crate |
| 8 | `build.sh` | C | Shell, not compiled; used when producing shared guest resources | Ad-hoc staging → canonical `modules/`, `tools/`, and `patch/` resource layout | Produced guest resources differ, so final bundle contents can differ | Retained because both hosts provision the same guest; hardware smoke remains required |
| 9 | `bundle.sh` | C | Not compiled; executed for macOS bundle generation | Older resource placement → canonical tools and generated merged dispatcher layout | Bundle resources intentionally differ | Kept shared; dual-profile validation now recognizes the macOS merged form |
| 10 | `crates/cdj3k-emu-firmware/src/initramfs.rs` | D | Yes / firmware provisioning | Upstream accepted the bundled macOS resources; frozen source required `patch-rootfs.d` even though the bundle emits a merged dispatcher; final source validates either a complete merged or complete directory profile | Frozen source could reject a valid macOS bundle; eliminated | WSL stays strict through its directory profile; macOS retains its merged profile |
| 11 | `crates/cdj3k-emu-platform/Cargo.toml` | B | Parsed on macOS; Linux dependency feature is not built for macOS | macOS dependency set → same set, plus target-scoped Linux portal features | No | Cargo target dependency boundary |
| 12 | `crates/cdj3k-emu-platform/src/audio_devices_linux.rs` | A | No / no | No Linux enumeration → Linux output-device enumeration | No | `cfg(target_os = "linux")` |
| 13 | `crates/cdj3k-emu-platform/src/lib.rs` | B | Yes / yes | Existing modules → additive Linux-only module export | No | `cfg(target_os = "linux")` |
| 14 | `crates/cdj3k-emu-platform/src/menu.rs` | D | Yes / native menu events execute | Native actions set upstream shade state; frozen shared handler removed it; final source routes actions by origin and restores native shade transitions while Linux actions remain shade-neutral | Frozen restart, service, audio, device, and network actions visibly changed; eliminated | `ActionOrigin` selects native versus Linux policy; transition tests cover both |
| 15 | `crates/cdj3k-emu-platform/src/menu_state.rs` | B | Yes / yes | Existing shared state → additive Linux diagnostic/start-stop fields and Linux-only audio refresh | Existing macOS fields and defaults remain | Linux enumeration is cfg-gated; additions are backward-compatible |
| 16 | `crates/cdj3k-emu-runtime/src/config.rs` | B | Yes / macOS `QemuConfig` executes | macOS HVF argv defaults → same defaults, plus separate Linux config types | Contract tests show unchanged macOS HVF, CPU, display, QMP, audio, and device defaults | Linux argv has a separate `LinuxQemuConfig` |
| 17 | `crates/cdj3k-emu-runtime/src/instance.rs` | D | Yes / launch, restart, stop, drop, cleanup | Upstream killed stale QEMU before directory creation, used only the eMMC lock, and removed the runtime directory on final stop; frozen source added a directory lock first and preserved the directory/logs; final source restores upstream macOS ordering and cleanup | Frozen source could block stale recovery and leave stopped slots appearing active; eliminated | Linux-only fields, flock, Unix QMP, diagnostic preservation, and transient cleanup are cfg-gated |
| 18 | `crates/cdj3k-emu-runtime/src/lib.rs` | B | Yes / yes | Existing exports → additive Linux config exports | No | Additive API only |
| 19 | `crates/cdj3k-emu-runtime/src/qmp.rs` | B | Yes / TCP QMP executes | TCP-only client → transport enum with unchanged TCP path plus Unix sockets | TCP behavior remains the macOS default | Linux opts into Unix sockets |
| 20 | `crates/cdj3k-emu-runtime/src/usb.rs` | B | Yes / runtime implementation unchanged | Cross-host unit-test compilation → BSD disk-path tests limited to macOS | No application behavior change | Test-only cfg correction |
| 21 | `crates/cdj3k-emu-runtime/src/vmnet.rs` | B | Yes / yes | Authorization and vmnet implementation → same implementation with platform guards and a non-macOS unsupported stub | No macOS command or authorization flow change | Non-macOS stub is selected by cfg |
| 22 | `crates/cdj3k-emu-storage/src/emmc.rs` | B | Yes / provisioning executes | Bundled `qemu-img` lookup → same lookup when optional override is `None` | No for default macOS construction | WSL alone supplies an explicit executable; default is contract-tested |
| 23 | `crates/cdj3k-emu-streams/src/control_gate.rs` | B | Yes / object is optional | No gate → additive launch-epoch gate | Legacy constructors remain immediate and ungated | WSL explicitly supplies a gate; macOS default is `None` |
| 24 | `crates/cdj3k-emu-streams/src/ctrl_stream.rs` | B | Yes / yes | Immediate socket connect → same legacy constructor plus optional readiness gate | No when gate is absent | Optional constructor parameter; legacy path tested |
| 25 | `crates/cdj3k-emu-streams/src/jog_stream.rs` | B | Yes / yes | Immediate SHM reader → same legacy constructor plus optional launch observation | No when gate is absent | Optional WSL gate; legacy constructor unchanged |
| 26 | `crates/cdj3k-emu-streams/src/lib.rs` | B | Yes / yes | Existing exports → additive gate and display-mode exports | No | Additive API only |
| 27 | `crates/cdj3k-emu-streams/src/main_stream.rs` | D | Yes / main display executes | Upstream accepted every changed generation and exposed the live mmap; frozen source accepted only unchanged even generations and always copied; final source defaults to `LegacyPublishedGeneration` and offers explicit `SequencedStableSnapshot` | Frozen source skipped odd macOS publications and changed copying, latency, and CPU behavior; eliminated | WSL explicitly selects sequenced owned snapshots; macOS retains mapped zero-copy and upstream frame accounting |
| 28 | `crates/cdj3k-emu-ui/src/app.rs` | D | Yes / main UI executes | Upstream started shaded, waited for 15 new frames, and preserved native action transitions; frozen source used Linux live-status behavior globally; final options default to legacy shade/display and WSL opts into Linux modes | Frozen launch/restart visuals and LCD blanking differed; eliminated | `BootShadeMode` and `MainDisplayMode` are explicit options; Linux View ordering fix is cfg-scoped |
| 29 | `crates/cdj3k-emu-ui/src/app/firmware_wizard.rs` | D | Yes / wizard executes | Completed provisioning restart forced upstream shade; frozen source removed it; final wizard defaults to legacy policy and accepts an explicit Linux policy | Frozen restart transition differed; eliminated | Wizard receives `BootShadeMode`; Linux selects live status |
| 30 | `crates/cdj3k-emu-ui/src/app/lcd_texture.rs` | D | Yes / GL upload executes | Upstream uploaded from mapped rows; frozen source required owned packed pixels; final source handles mapped legacy and owned sequenced payloads | Frozen source added a full shared copy path; eliminated for macOS | Payload enum selects zero-copy versus packed copy without a GL protocol guess |
| 31 | `docker/Dockerfile` | C | Not compiled or executed by the app; used to build shared guest artifacts | Guest build environment changed for new kernel/shim artifacts | Final guest artifacts can differ on macOS | Retained as shared build input; guest runtime requires hardware validation |
| 32 | `docs/linux-wsl.md` | E | Not compiled or executed | New WSL documentation | No | None needed |
| 33 | `docs/wsl-validation.md` | E | Not compiled or executed | New WSL evidence record | No | None needed |
| 34 | `guest/Makefile` | C | Host app does not compile it; guest build does; resulting shim executes on macOS guests | Shim source/link set expanded, including pthread support and shared jog contract | Yes, inside the guest | Shared because both hosts boot the same guest; Mac smoke required |
| 35 | `guest/ep122_shim/clock.c` | C | Built for ARM guest; executes under macOS QEMU | Time handling made portable for the guest ABI | Potential guest timing difference | Host-independent portability fix retained; Mac smoke required |
| 36 | `guest/ep122_shim/ep122_shim.h` | C | Built for ARM guest; definitions used by shim | Local jog constants → shared exact jog-mode/topology contract | Guest display contract is intentionally stricter | Shared contract retained; Mac smoke required |
| 37 | `guest/ep122_shim/jog.c` | C | Built for ARM guest; executes under macOS QEMU | Broad SETCRTC interception → exact mode/topology validation before emulation | Jog setup behavior can differ | Host-independent guest correctness change; Mac smoke required |
| 38 | `guest/ep122_shim/jog_contract.c` | C | Built for ARM guest; executes under macOS QEMU | No centralized contract → exact mode/topology predicates | Yes, guest validation is new | Shared because the guest topology is host-independent; Mac smoke required |
| 39 | `guest/ep122_shim/jog_drm.c` | C | Built for ARM guest; executes under macOS QEMU | Synthesized missing DSI-2 mode → verifies exact kernel-provided mode | Connector handling differs | Shared kernel/shim contract; Mac smoke required |
| 40 | `guest/ep122_shim/jog_shm.c` | C | Built for ARM guest; executes under macOS QEMU | Event-driven publication → serialized publication plus a 60 Hz worker and GLIBC 2.17 bindings | Jog cadence and guest CPU behavior can differ | Required for reliable shared guest output; Mac smoke required |
| 41 | `guest/ep122_shim/link.c` | C | Built for ARM guest; executes under macOS QEMU | Default pthread symbol → GLIBC 2.17-bound worker creation | ABI linkage changes, intended to improve compatibility | Host-independent guest ABI fix; Mac smoke required |
| 42 | `guest/kernel-patches/02-virtgpu-display-dsi2.patch` | C | Applied to guest kernel; kernel executes under macOS QEMU | Earlier connector behavior → exact synthetic DSI-2 detection/mode contract | Guest DRM topology differs | Shared guest kernel requirement; Mac smoke required |
| 43 | `guest/kernel-patches/cdj3k_jog_mode.h` | C | Included in guest kernel and shim builds | No common definition → single exact jog contract | Guest mode definition changes | Shared by design to prevent kernel/shim drift; Mac smoke required |
| 44 | `initramfs-patch/patch-rootfs.d/01-sn65-stub.sh` | B | Shell script executes during macOS provisioning | BSD-only `sed -i ''` usage → portable backup-suffix edit with cleanup | Intended output is unchanged | Platform-neutral portability fix |
| 45 | `initramfs-patch/patch-rootfs.d/03-dropbear-enable.sh` | B | Executes only when SSH patching is enabled | BSD-only in-place edits → portable backup-suffix edits | Intended output is unchanged | Platform-neutral portability fix |
| 46 | `initramfs-patch/patch-rootfs.d/04-root-password.sh` | B | Executes only when SSH patching is enabled | BSD-only in-place edit → portable backup-suffix edit | Intended output is unchanged | Platform-neutral portability fix |
| 47 | `initramfs-patch/patch-rootfs.d/21-cfgd.sh` | C | Executes during macOS provisioning | Search/fallback cfgd lookup → canonical `tools/cfgd` lookup | Resource-resolution failures now fail explicitly | Retained shared canonical layout; macOS bundle supplies that path |
| 48 | `initramfs-patch/patch-rootfs.sh` | C | Executes during macOS provisioning | Fixed local asset assumptions → explicit/default asset and tools roots | Invocation environment is more explicit | Shared dispatcher supports both validated profiles |
| 49 | `qemu/build.sh` | B | Not compiled; executed by the macOS QEMU build | Globbed top-level patches → explicit ordered manifest of the same 12 patches | Intended QEMU source result is unchanged; accidental WSL-patch exposure is removed | Builder ignores subdirectories and fails on a missing expected patch |
| 50 | `qemu/patches/wsl/wsl-04-shm-display-qapi-v10.2.2.patch` | A | No / no | New WSL-only QEMU patch | No | Separate WSL manifest and pinned QEMU revision |
| 51 | `qemu/patches/wsl/wsl-05-shm-display-meson-v10.2.2.patch` | A | No / no | New WSL-only QEMU patch | No | Separate WSL manifest and pinned QEMU revision |
| 52 | `qemu/patches/wsl/wsl-06-shm-display-source-v10.2.2.patch` | A | No / no | New odd/even WSL SHM writer | No, because original patch series cannot see it | Separate directory and explicit WSL manifest |
| 53 | `scripts/wsl/build-qemu.sh` | A | No / no | New pinned WSL QEMU builder | No | Linux-only script and explicit manifest |
| 54 | `scripts/wsl/build-resources.sh` | A | No / no | New WSL resource builder | No | Linux-only script |
| 55 | `scripts/wsl/preflight.sh` | A | No / no | New WSL environment checks | No | Linux-only script |
| 56 | `scripts/wsl/resource-layout.sh` | A | No / no | New strict directory-profile validator | No | Linux-only script; does not constrain macOS merged bundle |
| 57 | `scripts/wsl/run.sh` | A | No / no | New WSL launcher | No | Linux-only script |
| 58 | `scripts/wsl/test.sh` | A | No / no | New WSL static/runtime tests | No | Linux-only script |
| 59 | `scripts/wsl/toolchain.sh` | A | No / no | New WSL toolchain gate | No | Linux-only script |

## 4. Confirmed regressions, resolutions, and proof

### 4.1 Main-display publication protocol and ownership

- **Upstream contract:** the original macOS QEMU patch increments generation once after each completed dirty blit. Every changed value, odd or even, is a published frame. The Rust reader counts the generation immediately and passes the live mmap with the original surface stride to OpenGL.
- **Frozen WSL source:** the shared reader treated only a nonzero, unchanged even generation as stable and returned an owned packed copy. Odd publications from the unchanged macOS writer could be skipped, and the macOS zero-copy path was removed.
- **Why incompatible:** the parity bit has no “writer active” meaning in the original writer. Treating odd as unstable discards valid macOS publications.
- **Minimal fix:** `MainDisplayMode::LegacyPublishedGeneration` is the default for `MainLcdStream` and `CdjAppOptions`; `SequencedStableSnapshot` is an explicit WSL option. `DisplayPayload` distinguishes mapped surface-stride data from owned packed data, and the GL uploader supports both.
- **Proof:** deterministic tests cover legacy odd/even acceptance, the upstream initial-generation wait and immediate frame accounting, sequenced initial-even and even-only acceptance, before/after generation stability, dirty accumulation, packed row bytes, payload ownership, stride, and constructor defaults. WSL smoke observed a stable even 1280×720 nonblack frame twice.

### 4.2 Boot shade, native actions, and firmware-wizard restart

- **Upstream contract:** macOS starts fully shaded with LCDs blanked; a QEMU transition records a frame baseline; the overlay clears after 15 fresh main frames. Restart, service mode, audio changes, audio-device changes, network changes, and the wizard's Restart action force the shade.
- **Frozen WSL source:** the shared application started unshaded, reduced booting to `shade_forced`, and removed shade transitions from the shared menu and wizard.
- **Why incompatible:** macOS launch and restart visuals, LCD blanking, and transition gating became observably different.
- **Minimal fix:** `BootShadeMode::LegacyFrameThreshold` is the default. `LinuxLiveStatus` is an explicit WSL option. Menu actions carry an internal `ActionOrigin`; native actions restore upstream shade transitions while Linux actions remain shade-neutral. The wizard receives the same mode. Linux draws its operator bar before polling state, fixing the three same-frame View toggles without changing macOS ordering.
- **Proof:** tests cover the initial state, 15-frame threshold, baseline arithmetic, native restart/service/audio/device/network actions, Linux shade neutrality, wizard defaults and Linux policy, and independent External Jog, External Main, and Debug toggles.

### 4.3 Instance lifecycle, lock ordering, and cleanup

- **Upstream contract:** macOS attempts stale-QEMU recovery before creating the runtime directory, holds only the eMMC lock, guards shutdown by monitor-thread presence, preserves vmnet only during restart cleanup, and removes the entire runtime directory on final cleanup.
- **Frozen WSL source:** macOS first created and locked the runtime directory, shared Linux's shutdown short-circuit, and preserved the directory and diagnostics during final cleanup.
- **Why incompatible:** the new lock could prevent upstream stale recovery, and the persistent directory made `instance_dir.exists()` report a stopped slot as running.
- **Minimal fix:** macOS spawn, restart, stop, drop, and cleanup again follow the upstream structure. Linux instance locking, Unix QMP paths, serial diagnostics, and transient-only cleanup are cfg-gated Linux fields and functions.
- **Proof:** tests verify macOS restart cleanup preserves vmnet while deleting all other contents and zeroing mapped SHM magic; final cleanup removes the complete directory; Linux tests verify lock exclusion, stale Unix-QMP removal, diagnostic preservation, and transient cleanup.

### 4.4 Firmware resource profile mismatch

- **Upstream contract:** a generated macOS app contains a self-contained merged `patch-rootfs.sh`; it does not need a sibling `patch-rootfs.d` directory at runtime.
- **Frozen WSL source:** shared validation required `patch/patch-rootfs.d`, which matches WSL's directory resources but not the macOS merged bundle.
- **Why incompatible:** the firmware wizard could reject a complete macOS application bundle before provisioning.
- **Minimal fix:** validation identifies either `MergedDispatcher` or `DirectoryDispatcher`. A merged dispatcher must have its generated signature, completion marker, and all 18 step markers and must not have a scripts directory. A directory dispatcher must reference the directory and contain every expected numbered step. Hybrid and truncated trees are rejected.
- **Proof:** tests accept both complete profiles and reject missing canonical resources, truncated merged dispatchers, missing directory steps, and hybrid layouts. `FirmwareWizard::new()` and `EmmcConfig::new()` retain bundled-resource and bundled-`qemu-img` defaults.

## 5. Shared changes that remain

The category-C guest changes remain because both hosts boot the same guest-side kernel, initramfs, and EP122 shim. The exact jog-mode header deliberately binds the kernel and shim to one topology; SETCRTC validation, connector checks, the serialized 60 Hz jog publisher, GLIBC 2.17 bindings, portable time calls, and pthread linkage are internally coherent and passed WSL runtime validation.

Those facts do not prove behavior under HVF. They can affect boot, DRM enumeration, jog cadence, timing, and EP122 behavior on a Mac. They were not converted to host conditionals because the code executes in the guest and the intended device contract is host-independent. They remain **MODERATE** risk until the hardware matrix below passes.

The canonical provisioning layout is also shared. It remains because both resource profiles require the same semantic modules, tools, and patch assets. The profile-specific difference is isolated at validation: WSL uses a directory dispatcher and macOS uses the generated merged dispatcher.

## 6. Final macOS risk matrix

Risk describes the final source at `2de7994d7c3b5b93a97e0b0e6de7a9719e3d4fd4`, after remediation. `NONE` is used only where static evidence completely excludes the path; absence of a Mac runtime result otherwise leaves at least LOW risk.

| Component | Upstream behavior | Final review behavior | Mac-visible? | Risk | Resolution/evidence | Runtime test still needed? |
| --- | --- | --- | :---: | :---: | --- | :---: |
| QEMU patch selection | Original 12 patches | Explicit same 12-patch manifest; WSL subtree excluded | Yes, at build | LOW | Names/order, independent applicability, and SHA-256 identity passed | Yes |
| HVF configuration | HVF with host CPU and original machine args | Same macOS `QemuConfig` defaults | Yes | LOW | Darwin implementation unchanged; argv contract test passed | Yes |
| CoreAudio bypass | Original patched CoreAudio/bypass path | Same original patches and macOS argv selection | Yes | LOW | Patch contents unchanged; no shared audio rewrite | Yes |
| QMP TCP | TCP loopback QMP | Same TCP client; Unix transport additive for Linux | Yes | LOW | TCP contract retained and tested statically | Yes |
| vmnet | Original socket-vmnet lifecycle | Same macOS implementation; cfg guards only | Yes | LOW | Direct implementation unchanged; restart socket contract restored | Yes |
| AuthorizationServices | Native elevation dialog and privileged launch | Same macOS FFI and command path | Yes | LOW | macOS implementation unchanged | Yes |
| physical USB | Original macOS disk and eject flow | Runtime flow unchanged | Yes | LOW | Only test compilation was cfg-corrected | Yes |
| runtime cleanup | Final stop removes entire directory | Restored; Linux alone preserves diagnostics | Yes | LOW | Deterministic full-removal test passed | Yes |
| instance locking | eMMC lock only | Restored on macOS; directory flock Linux-only | Yes | LOW | cfg-separated structure and lock tests | Yes |
| restart | Upstream stop, unlock, spawn, vmnet preservation | Restored field ownership and cleanup order | Yes | LOW | Restart cleanup tests passed | Yes |
| shutdown | Monitor-thread presence controls sequence | Restored on macOS; Linux retains reaped-child short-circuit | Yes | LOW | Source behavior matches upstream; WSL fallback is Linux-side | Yes |
| boot shade | Initial shade and 15-frame threshold | Restored by default | Yes | LOW | State-transition tests passed | Yes |
| menus | Native actions force upstream transitions | Restored by native action origin | Yes | LOW | Action tests passed | Yes |
| firmware wizard | Bundled defaults and shaded restart | Restored; optional Linux inputs are opt-in | Yes | LOW | Constructor and restart-policy tests passed | Yes |
| resource layout | Complete merged app resources | Complete merged or directory semantic profile | Yes | MODERATE | Static profile tests pass; final app must be built and provisioned on Mac | Yes |
| qemu-img lookup | Bundled tool | Same when `qemu_img == None` | Yes | LOW | `EmmcConfig::new()` default test passed | Yes |
| main display stream | Every changed generation, live mmap | Same default; WSL explicitly uses sequenced owned copy | Yes | LOW | Protocol/payload tests pass; no GL-context runtime test | Yes |
| jog display | Existing guest publication path | Shared exact topology and 60 Hz publisher | Yes | MODERATE | Coherent and WSL-proven, not HVF-proven | Yes |
| controls | Immediate legacy connection | Same when gate is absent | Yes | LOW | Legacy constructor contract preserved; WSL gate opt-in | Yes |
| guest kernel | Prior guest DRM behavior | Shared DSI-2 detection/mode changes | Yes, inside guest | MODERATE | WSL-proven only | Yes |
| EP122 shim | Prior interception/publication behavior | Exact validation, ABI bindings, publisher worker | Yes, inside guest | MODERATE | WSL-proven only | Yes |
| external viewports | Shared upstream viewport implementation | Unchanged viewport implementation; Linux event ordering fixed | Yes | LOW | No macOS ordering change; Linux toggle tests pass | Yes |
| multi-instance menu state | Runtime-directory existence denotes running | Final macOS cleanup again removes directory | Yes | LOW | Cleanup contract test eliminates persistent false-running state | Yes |
| bundle generation | Original macOS app assembly | Canonical tools plus complete merged dispatcher | Yes | MODERATE | Static profile and patch tests pass; signing/bundle runtime untested | Yes |

## 7. Exact remaining Mac smoke matrix

Every item below is **REQUIRES MAC HARDWARE** and must be run against source commit `2de7994d7c3b5b93a97e0b0e6de7a9719e3d4fd4` (or a later commit containing documentation only).

1. **REQUIRES MAC HARDWARE** — clean original QEMU build.
2. **REQUIRES MAC HARDWARE** — macOS application build.
3. **REQUIRES MAC HARDWARE** — bundle generation/signing as appropriate.
4. **REQUIRES MAC HARDWARE** — firmware wizard opens.
5. **REQUIRES MAC HARDWARE** — firmware provisioning completes.
6. **REQUIRES MAC HARDWARE** — eMMC creation.
7. **REQUIRES MAC HARDWARE** — first boot.
8. **REQUIRES MAC HARDWARE** — main display.
9. **REQUIRES MAC HARDWARE** — jog display.
10. **REQUIRES MAC HARDWARE** — controls.
11. **REQUIRES MAC HARDWARE** — native menus.
12. **REQUIRES MAC HARDWARE** — Service Mode restart.
13. **REQUIRES MAC HARDWARE** — normal Restart Emulation.
14. **REQUIRES MAC HARDWARE** — stop/relaunch.
15. **REQUIRES MAC HARDWARE** — external main viewport.
16. **REQUIRES MAC HARDWARE** — external jog viewport.
17. **REQUIRES MAC HARDWARE** — debug viewport.
18. **REQUIRES MAC HARDWARE** — CoreAudio device enumeration.
19. **REQUIRES MAC HARDWARE** — sustained CoreAudio/bypass playback.
20. **REQUIRES MAC HARDWARE** — audio-device switch/restart.
21. **REQUIRES MAC HARDWARE** — ALC.
22. **REQUIRES MAC HARDWARE** — vmnet host mode.
23. **REQUIRES MAC HARDWARE** — bridged/vmnet behavior.
24. **REQUIRES MAC HARDWARE** — multi-instance launch.
25. **REQUIRES MAC HARDWARE** — multi-instance menu state.
26. **REQUIRES MAC HARDWARE** — clean shutdown.
27. **REQUIRES MAC HARDWARE** — subsequent relaunch.

## 8. Git and validation lineage

| Revision | Meaning | Validation that applies |
| --- | --- | --- |
| `d1a25ef188a01729c3637b6c2e9d7d1bebb69f99` | Upstream macOS compatibility baseline | Behavioral oracle for this audit |
| `26d0f453aa68e308d1c19ad08de9ce262def1aa2` | Frozen WSL runtime-validation source | Original three-attempt WSL result; contains the four host-side compatibility regressions described above |
| `2de7994d7c3b5b93a97e0b0e6de7a9719e3d4fd4` (`2de7994`) | macOS-contract-remediated source and tests | Clean committed-tree source validation plus one fresh WSL launch/relaunch smoke after rebuilding current QEMU |

The original 3/3 result belongs to `26d0f453...`. The new launch/relaunch smoke bridges the runtime evidence to `2de7994d7c3b5b93a97e0b0e6de7a9719e3d4fd4`; it does not constitute a new three-attempt matrix. This report is documentation and does not change the source boundary.

## 9. Nicolas-facing summary

I did a second pass specifically for macOS regression risk. I found four places where the WSL work had leaked into shared behavior: the main-display generation protocol and ownership model, the boot-shade/native-menu lifecycle, runtime-directory locking and cleanup, and firmware resource-profile validation.

Those paths now default to the upstream macOS contracts, while WSL explicitly opts into its sequenced display reader, Linux live-status shade, instance locking, Unix QMP, and diagnostic-preserving cleanup. The original macOS QEMU patch series applies cleanly in its exact 12-patch order, its patch contents are unchanged, and the Darwin HVF/CoreAudio/vmnet/AuthorizationServices implementations remain unchanged.

The final source passed workspace, contract, manifest, patch-applicability, and WSL validation. The rebuilt WSL QEMU passed an end-to-end launch/relaunch smoke covering QMP/control connection, stable main and jog display, transient cleanup, and relaunch. The remaining shared guest kernel and EP122-shim changes are internally coherent and WSL-proven, but they still require the documented 27-step smoke matrix on Mac hardware. I am therefore not claiming macOS runtime non-regression yet.

## 10. Final conclusion

> The four identified host-side macOS regressions were removed by restoring upstream-default semantics and moving WSL-specific behavior behind explicit opt-in paths. Original Darwin HVF/CoreAudio/vmnet implementation and QEMU patch contents remain unchanged. Source-level contract tests cover the areas that can be validated without Apple hardware. Shared guest-side kernel/shim changes remain and therefore macOS runtime non-regression is not claimed until the documented Mac smoke matrix is executed.
