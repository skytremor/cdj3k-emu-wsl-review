#!/usr/bin/env bash
set -euo pipefail

# Canonical non-proprietary WSL resource layout. Firmware, keys, eMMC images,
# and extracted rootfs data are intentionally outside this directory.
resource_files=(
  Image
  modules/subucom_virt.ko
  modules/virtio_snd.ko
  modules/udev_usb1.ko
  tools/ep122_shim.so
  tools/subucom_forwarder
  tools/subucom_live
  tools/cfgd
  patch/patch-rootfs.sh
  patch/dummy_drv.so
  patch/vanilla-modules/subucom_virt.ko
  patch/vanilla-modules/virtio_snd.ko
  patch/vanilla-modules/udev_usb1.ko
)
resource_dirs=(
  patch/patch-rootfs.d
)

validate_resource_layout() {
  local root=${1:?resource root required}
  if [[ -f "${root}/ep122_shim.so" && ! -d "${root}/tools" ]]; then
    echo "stale flat WSL resource layout: ${root}; rebuild with scripts/wsl/build-resources.sh" >&2
    return 1
  fi
  local missing=0
  local relative
  for relative in "${resource_files[@]}"; do
    if [[ ! -f "${root}/${relative}" ]]; then
      echo "missing resource: ${root}/${relative}" >&2
      missing=1
    fi
  done
  for relative in "${resource_dirs[@]}"; do
    if [[ ! -d "${root}/${relative}" ]]; then
      echo "missing resource directory: ${root}/${relative}" >&2
      missing=1
    fi
  done
  return "${missing}"
}
