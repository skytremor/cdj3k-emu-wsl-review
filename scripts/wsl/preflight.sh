#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "${ROOT_DIR}/scripts/wsl/resource-layout.sh"

QEMU="${ROOT_DIR}/build/wsl-qemu-install/bin/qemu-system-aarch64"
QEMU_IMG="${ROOT_DIR}/build/wsl-qemu-install/bin/qemu-img"
RESOURCES="${ROOT_DIR}/build/wsl-resources"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --qemu) QEMU=${2:?--qemu requires a path}; shift 2 ;;
    --qemu-img) QEMU_IMG=${2:?--qemu-img requires a path}; shift 2 ;;
    --resources) RESOURCES=${2:?--resources requires a directory}; shift 2 ;;
    *) echo "unknown preflight option: $1" >&2; exit 2 ;;
  esac
done

failures=0
check_exec() {
  local label=$1 value=$2
  if [[ "$value" == */* ]]; then
    [[ -x "$value" ]] || { echo "missing executable $label: $value" >&2; failures=$((failures + 1)); }
  else
    command -v "$value" >/dev/null || { echo "missing executable $label: $value" >&2; failures=$((failures + 1)); }
  fi
}

check_exec "QEMU" "${QEMU}"
check_exec "qemu-img" "${QEMU_IMG}"
if ! validate_resource_layout "${RESOURCES}"; then
  failures=$((failures + 1))
fi

if [[ -z "${WAYLAND_DISPLAY:-}" ]]; then
  echo "missing WSLg display: WAYLAND_DISPLAY is not set" >&2
  failures=$((failures + 1))
fi
if [[ -z "${XDG_RUNTIME_DIR:-}" || ! -d "${XDG_RUNTIME_DIR}" ]]; then
  echo "missing WSLg runtime: XDG_RUNTIME_DIR is not a directory" >&2
  failures=$((failures + 1))
fi

if (( failures > 0 )); then
  exit 1
fi
echo "WSL host preflight passed; missing per-instance firmware is allowed on first run"
