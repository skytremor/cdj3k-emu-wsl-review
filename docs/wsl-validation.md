# WSL review validation

Status: **in progress; do not share as a reproduced firmware build yet**.

## Revision identity

| Revision | Full SHA |
| --- | --- |
| Upstream base (`origin/main` merge base) | `d1a25ef188a01729c3637b6c2e9d7d1bebb69f99` |
| Source cleanup commit | `7c6f6d55010d6ae6b24f3de45236655e0fa08956` |
| Final review HEAD | Pending the firmware-backed validation and final documentation commit |

The final validation record must name the full final review HEAD SHA after
that commit is frozen. Firmware attempts from an earlier source commit do not
validate a later one.

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

Environment: Ubuntu 24.04 on x86_64 WSL2, Rust/Cargo 1.97.1.

| Check | Result |
| --- | --- |
| Original 12-patch series on pinned QEMU `ee7eb612be8f8886d48c1d0c1f1c65e495138f83` | PASS: clean sequential applicability |
| WSL three-patch series on pinned QEMU `f8ed81651e61d9c2166df6121ce2af0f44f06b3e` | PASS: clean sequential applicability |
| WSL QEMU v10.2.2 fresh configure, compile, install | PASS with locally cached upstream subprojects at their pinned wrap revisions |
| Shell patch-selection and resource-staging tests | PASS |
| `git diff --check`, `cargo metadata --no-deps`, `cargo fmt --all --check` | PASS |
| Standalone control-readiness tests | PASS: 5/5 |
| `cargo test --workspace --locked` | BLOCKED: locked `mio 1.2.3` is not cached and crates.io cannot be reached |
| Guest-resource build and WSL preflight | BLOCKED: Docker Desktop's WSL integration is unavailable; existing ignored resources have the old flat layout |
| Firmware provisioning and three boot attempts | NOT RUN on this candidate |
| macOS runtime | NOT PERFORMED without a Mac |

The original QEMU build script now consumes only its explicit top-level
manifest. The WSL script consumes only its nested WSL manifest. No
Darwin-specific application source or original QEMU patch contents were
changed by the cleanup commit; shared display/control code and the original
QEMU build script did change, so a macOS runtime smoke test remains necessary.

## Remaining review gate

Restore locked Cargo dependency access and an arm64-capable Buildx builder,
then complete workspace tests, build canonical guest resources, and run the
firmware wizard with user-owned inputs outside Git. Freeze the candidate
commit before provisioning and perform three boots without tracked source
changes. Record X.Org, EP122, main/jog displays, controls, shutdown, relaunch,
and each attempt's failure signature. At least one complete pass is required
for technical review; 0/3 stops sharing.

Before making the review repository public, audit all refs and obtain a human
decision on inherited screenshots, GIFs, and icons. Keep firmware, keys,
derived images, raw logs, and local paths out of the published record.
