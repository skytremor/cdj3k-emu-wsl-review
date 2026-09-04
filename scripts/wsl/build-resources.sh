#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT_DIR="${ROOT_DIR}/build/wsl-resources"
SCRIPT_DIR="${ROOT_DIR}/scripts/wsl"
source "${SCRIPT_DIR}/resource-layout.sh"

STAGE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/cdj3k-wsl-resources.XXXXXX")"
trap 'rm -rf "${STAGE_DIR}"' EXIT

# The Docker target produces only non-proprietary kernel, module, shim, and
# helper inputs. Firmware, keys, eMMC images, and extracted rootfs data stay
# in the user's per-instance XDG data directory.
docker buildx build --platform linux/arm64 --target artifacts \
  --output "type=local,dest=${STAGE_DIR}" \
  -f "${ROOT_DIR}/docker/Dockerfile" "${ROOT_DIR}"

rm -rf "${OUT_DIR}"
mkdir -p "${OUT_DIR}/modules" "${OUT_DIR}/tools" \
  "${OUT_DIR}/patch/vanilla-modules"
cp "${STAGE_DIR}/Image" "${OUT_DIR}/Image"
cp "${STAGE_DIR}/modules/"*.ko "${OUT_DIR}/modules/"
cp "${STAGE_DIR}/modules/"*.ko "${OUT_DIR}/patch/vanilla-modules/"
cp "${STAGE_DIR}/ep122_shim.so" "${OUT_DIR}/tools/ep122_shim.so"
cp "${STAGE_DIR}/subucom_forwarder_aarch64" "${OUT_DIR}/tools/subucom_forwarder"
cp "${STAGE_DIR}/subucom_live_aarch64" "${OUT_DIR}/tools/subucom_live"
cp "${STAGE_DIR}/cfgd_aarch64" "${OUT_DIR}/tools/cfgd"
cp "${ROOT_DIR}/initramfs-patch/patch-rootfs.sh" "${OUT_DIR}/patch/patch-rootfs.sh"
cp -a "${ROOT_DIR}/initramfs-patch/patch-rootfs.d" "${OUT_DIR}/patch/"
cp "${STAGE_DIR}/dummy_drv.so" "${OUT_DIR}/patch/dummy_drv.so"

validate_resource_layout "${OUT_DIR}"
echo "WSL resources staged in ${OUT_DIR}"
