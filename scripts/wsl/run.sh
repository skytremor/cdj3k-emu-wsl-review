#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
QEMU="${ROOT_DIR}/build/wsl-qemu-install/bin/qemu-system-aarch64"
QEMU_IMG="${ROOT_DIR}/build/wsl-qemu-install/bin/qemu-img"
RESOURCES="${ROOT_DIR}/build/wsl-resources"
HAS_QEMU=0
HAS_QEMU_IMG=0
HAS_RESOURCES=0
HAS_VIRTUAL_MEDIA=0

APP_ARGS=("$@")
for ((index = 0; index < ${#APP_ARGS[@]}; index++)); do
  case "${APP_ARGS[index]}" in
    --qemu) QEMU=${APP_ARGS[index + 1]:?--qemu requires a path}; HAS_QEMU=1 ;;
    --qemu-img) QEMU_IMG=${APP_ARGS[index + 1]:?--qemu-img requires a path}; HAS_QEMU_IMG=1 ;;
    --resources) RESOURCES=${APP_ARGS[index + 1]:?--resources requires a directory}; HAS_RESOURCES=1 ;;
    --virtual-media) HAS_VIRTUAL_MEDIA=1 ;;
  esac
done

PREFLIGHT_ARGS=( \
  --qemu "${QEMU}" \
  --qemu-img "${QEMU_IMG}" \
  --resources "${RESOURCES}" \
)
if (( HAS_VIRTUAL_MEDIA )); then PREFLIGHT_ARGS+=(--virtual-media); fi
"${ROOT_DIR}/scripts/wsl/preflight.sh" "${PREFLIGHT_ARGS[@]}"

export LIBGL_ALWAYS_SOFTWARE="${LIBGL_ALWAYS_SOFTWARE:-1}"
export GALLIUM_DRIVER="${GALLIUM_DRIVER:-llvmpipe}"
if (( ! HAS_QEMU )); then APP_ARGS+=(--qemu "${QEMU}"); fi
if (( ! HAS_QEMU_IMG )); then APP_ARGS+=(--qemu-img "${QEMU_IMG}"); fi
if (( ! HAS_RESOURCES )); then APP_ARGS+=(--resources "${RESOURCES}"); fi
exec cargo run --manifest-path "${ROOT_DIR}/Cargo.toml" -p cdj3k-emu-wsl -- "${APP_ARGS[@]}"
