#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT_DIR="${ROOT_DIR}/build/wsl-resources"
SCRIPT_DIR="${ROOT_DIR}/scripts/wsl"
source "${SCRIPT_DIR}/resource-layout.sh"

mkdir -p "${ROOT_DIR}/build"
STAGE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/cdj3k-wsl-resources.XXXXXX")"
NEXT_DIR="$(mktemp -d "${ROOT_DIR}/build/.wsl-resources.next.XXXXXX")"
OLD_DIR="${NEXT_DIR}.previous"
cleanup() {
  if [[ -e "${OLD_DIR}" && ! -e "${OUT_DIR}" ]]; then
    mv "${OLD_DIR}" "${OUT_DIR}"
  fi
  rm -rf "${STAGE_DIR}" "${NEXT_DIR}" "${OLD_DIR}"
}
trap cleanup EXIT

# The Docker target produces only non-proprietary kernel, module, shim, and
# helper inputs. Firmware, keys, eMMC images, and extracted rootfs data stay
# in the user's per-instance XDG data directory.
docker buildx build --platform linux/arm64 --target artifacts \
  --output "type=local,dest=${STAGE_DIR}" \
  -f "${ROOT_DIR}/docker/Dockerfile" "${ROOT_DIR}"

mkdir -p "${NEXT_DIR}/modules" "${NEXT_DIR}/tools" \
  "${NEXT_DIR}/patch/vanilla-modules"
cp "${STAGE_DIR}/Image" "${NEXT_DIR}/Image"
cp "${STAGE_DIR}/modules/"*.ko "${NEXT_DIR}/modules/"
cp "${STAGE_DIR}/modules/"*.ko "${NEXT_DIR}/patch/vanilla-modules/"
cp "${STAGE_DIR}/ep122_shim.so" "${NEXT_DIR}/tools/ep122_shim.so"
cp "${STAGE_DIR}/subucom_forwarder_aarch64" "${NEXT_DIR}/tools/subucom_forwarder"
cp "${STAGE_DIR}/subucom_live_aarch64" "${NEXT_DIR}/tools/subucom_live"
cp "${STAGE_DIR}/cfgd_aarch64" "${NEXT_DIR}/tools/cfgd"
cp "${ROOT_DIR}/initramfs-patch/patch-rootfs.sh" "${NEXT_DIR}/patch/patch-rootfs.sh"
cp -a "${ROOT_DIR}/initramfs-patch/patch-rootfs.d" "${NEXT_DIR}/patch/"
cp "${STAGE_DIR}/dummy_drv.so" "${NEXT_DIR}/patch/dummy_drv.so"

validate_resource_layout "${NEXT_DIR}"
if [[ -e "${OUT_DIR}" ]]; then
  mv "${OUT_DIR}" "${OLD_DIR}"
fi
if ! mv "${NEXT_DIR}" "${OUT_DIR}"; then
  [[ ! -e "${OLD_DIR}" ]] || mv "${OLD_DIR}" "${OUT_DIR}"
  exit 1
fi
rm -rf "${OLD_DIR}"
echo "WSL resources staged in ${OUT_DIR}"
