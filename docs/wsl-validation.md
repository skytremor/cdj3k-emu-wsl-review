# WSL review validation

Status: **3/3 firmware boots reproduced; publication remains gated on the
product-imagery rights decision and anonymous-access check**.

## Revision identity

| Revision | Full SHA |
| --- | --- |
| Upstream base (`origin/main` merge base) | `d1a25ef188a01729c3637b6c2e9d7d1bebb69f99` |
| Source cleanup commit | `7c6f6d55010d6ae6b24f3de45236655e0fa08956` |
| Frozen/tested source commit | `26d0f453aa68e308d1c19ad08de9ce262def1aa2` |

All provisioning and boot attempts below used that exact source commit. No
tracked file changed between the three attempts. This validation-document
update is a documentation-only successor to the tested source commit.

## Historical evidence

The original WSL lab recorded three successful August 20 boots: first real
main UI at 45.303–48.426 s and settled/control-ready state at 48.338–51.172 s
on its QEMU/readiness clock. A separate passing interaction test recorded
89 ms shortcut and 251 ms touch visual response. A later readiness gate passed
only 1/3 attempts. These are historical measurements, not results from this
review commit; the archived run manifests do not identify an exact producing
source SHA. Audio playback and physical DJ Link were not demonstrated by those
records.

## Current source checks (2026-09-22)

Environment: Ubuntu 24.04 on x86_64 WSL2. The locked workspace test used
Rust/Cargo 1.88.0; preflight also passed with Rust/Cargo 1.97.1. The resource
build used Docker 28.3.2 and BuildKit 0.23.2 with `linux/arm64` emulation.
Native arm64 was not required. The first emulated kernel build took about
80 minutes. The builder needed network access to Docker Hub, kernel.org, and
ports.ubuntu.com.

| Check | Result |
| --- | --- |
| Original 12-patch series on pinned QEMU `ee7eb612be8f8886d48c1d0c1f1c65e495138f83` | PASS: clean sequential applicability |
| WSL three-patch series on pinned QEMU `f8ed81651e61d9c2166df6121ce2af0f44f06b3e` | PASS: clean sequential applicability |
| WSL QEMU v10.2.2 fresh configure, compile, install | PASS with locally cached upstream subprojects at their pinned wrap revisions |
| Shell patch-selection and resource-staging tests | PASS |
| `git diff --check`, `cargo metadata --no-deps --locked`, `cargo fmt --all --check` | PASS |
| Standalone control-readiness tests | PASS: 5/5 |
| `cargo test --workspace --locked` | PASS: 19/19 unit tests; doc-test suites also passed |
| Guest-resource build | PASS: pinned Linux v6.6.138, three out-of-tree modules, guest tools, X.Org dummy driver, and glibc shim built from source and staged in the canonical resource tree |
| WSL preflight | PASS with the fresh canonical resource tree, QEMU 10.2.2 `shm` backend, WSLg, and Cargo newer than 1.85 |
| Firmware provisioning and three boot attempts | PASS: provisioned once from the frozen source commit; 3/3 boots completed |
| macOS runtime | NOT PERFORMED without a Mac |

The original QEMU build script now consumes only its explicit top-level
manifest in listed order and ignores every subdirectory. The WSL script
consumes only its explicit three-patch WSL manifest. Both series applied to
their pinned pristine QEMU revisions. No original QEMU patch contents were
changed. Shared firmware extraction, runtime lifecycle, display, and control
code did change across the review branch, so a macOS runtime smoke test remains
recommended and is explicitly unverified here.

## Final-candidate firmware results (2026-09-22)

Provisioning used user-owned inputs outside Git and called the same public
firmware APIs as the wizard in the same order. `LuksKey::from_file` confirmed
that the input is a keyfile path. The keyfile, firmware update, generated eMMC,
patched firmware files, screenshots, and raw logs remained outside the
repository. No local input path or secret value is recorded here.

All three attempts used the frozen source SHA above and the same freshly built
QEMU and resource tree. Audio and networking were disabled to isolate the boot,
display, and control path.

| Attempt | Launch path | X.Org | EP122 application | Jog readiness | Control | Main semantic frame | Result |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| 1 | Fresh application launch | +14.964 s | +26.090 s | +26.507 s | +28.017 s | +71.583 s; 813,717 nonblack pixels, 481 colors | PASS |
| 2 | Built-in **Restart Emulation** | guest +15.283 s | guest +24.111 s | publisher active at guest +24.359 s | reconnected; received 7,493 MOSI frames before shutdown | checked after settle; 813,339 nonblack pixels, 510 colors | PASS |
| 3 | Full application relaunch | +15.073 s | +23.528 s | +23.962 s | +25.974 s; received 3,965 MOSI frames before shutdown | +68.693 s; 812,800 nonblack pixels, 268 colors | PASS |

The `+` values are host monotonic times from observed QEMU spawn. Attempt 2's
guest values come from the new serial log created by the in-process restart;
that restart was not independently armed with the host timeline monitor, so no
host time is claimed for its settled main frame or control connection.

Each boot produced a 1280×720 XRGB main framebuffer, a valid 320×240 jog SHM
header, continuing stable jog sequence updates, and a host control connection.
The MOSI frame counts demonstrate guest-to-host control traffic. Graceful close
sent the power-off control frame back to the guest; every serial log recorded
the forwarder's power-off detection. Both full closes entered systemd shutdown,
removed transient QMP/control/display files, released the instance and eMMC
locks, and allowed a clean relaunch.

The fully instrumented main semantic frames appeared at 68.693 and 71.583
seconds, 17.5–20.4 seconds later than the best historical 45–51 second window.
EP122, jog, and control readiness completed by 28.017 seconds in attempt 1 and
25.974 seconds in attempt 3. This is a reliability/performance note rather than
a failed boot: all required paths completed without a timeout, and the evidence
did not identify a readiness or control defect. No readiness/control behavior
was changed during this validation.

## Recorded follow-up

Optional virtual media still emits the Linux eMMC block device before the USB
device even though the firmware probes virtio-mmio block devices in reverse
order. This predates the final-candidate fixes and was outside the basic boot
gate; review and test it separately before claiming virtual-media support.

## Remaining sharing gate

The tracked history and refs were audited for secrets and prohibited generated
artifacts. Before making the review repository public, obtain a human rights
decision for the inherited screenshots, GIFs, and product imagery. After that
decision, make the review URL public and verify it in an unauthenticated
session. Keep firmware, keys, derived images, raw logs, and local paths out of
the published record.
