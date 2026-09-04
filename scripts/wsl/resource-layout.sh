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

validate_resource_layout() {
  local root=${1:?resource root required}
  local missing=0
  local relative
  for relative in "${resource_files[@]}"; do
    if [[ ! -f "${root}/${relative}" ]]; then
      echo "missing resource: ${root}/${relative}" >&2
      missing=1
    fi
  done
  return "${missing}"
}
