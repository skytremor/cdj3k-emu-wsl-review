#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
for script in build-qemu.sh build-resources.sh preflight.sh resource-layout.sh run.sh toolchain.sh; do
  bash -n "${ROOT_DIR}/scripts/wsl/${script}"
done
bash -n "${ROOT_DIR}/qemu/build.sh"

mapfile -t original_patches < <(bash "${ROOT_DIR}/qemu/build.sh" --list-patches)
mapfile -t wsl_patches < <(bash "${ROOT_DIR}/scripts/wsl/build-qemu.sh" --list-patches)
test "${#original_patches[@]}" -eq 12
test "${#wsl_patches[@]}" -eq 3
for index in "${!original_patches[@]}"; do
  patch=${original_patches[index]}
  [[ "${patch}" == "${ROOT_DIR}/qemu/patches/"[0-9][0-9]-*.patch ]]
  expected=$(printf '%02d' "$((index + 1))")
  [[ "${patch##*/}" == "${expected}-"* ]]
done
for patch in "${wsl_patches[@]}"; do
  [[ "${patch}" == "${ROOT_DIR}/qemu/patches/wsl/wsl-"*.patch ]]
done

source "${ROOT_DIR}/scripts/wsl/toolchain.sh"
version_at_least_1_85 1.85.0
version_at_least_1_85 1.97.1
! version_at_least_1_85 1.75.0
! version_at_least_1_85 invalid

source "${ROOT_DIR}/scripts/wsl/resource-layout.sh"
test "${#resource_files[@]}" -gt 0

fixture="$(mktemp -d "${TMPDIR:-/tmp}/cdj3k-resource-layout.XXXXXX")"
trap 'rm -rf "${fixture}"' EXIT
for relative in "${resource_files[@]}"; do
  mkdir -p "${fixture}/$(dirname "${relative}")"
  touch "${fixture}/${relative}"
done
for relative in "${resource_dirs[@]}"; do
  mkdir -p "${fixture}/${relative}"
done
validate_resource_layout "${fixture}"
rm "${fixture}/tools/cfgd"
if validate_resource_layout "${fixture}" 2>/dev/null; then
  echo "resource validator accepted an incomplete fixture" >&2
  exit 1
fi
touch "${fixture}/tools/cfgd"
rm -r "${fixture}/patch/patch-rootfs.d"
if validate_resource_layout "${fixture}" 2>/dev/null; then
  echo "resource validator accepted a missing patch directory" >&2
  exit 1
fi

old_layout="${fixture}/old-layout"
mkdir -p "${old_layout}/modules"
touch "${old_layout}/Image" "${old_layout}/ep122_shim.so" "${old_layout}/cfgd_aarch64"
if validate_resource_layout "${old_layout}" 2>"${fixture}/error"; then
  echo "resource validator accepted the old flat layout" >&2
  exit 1
fi
grep -q 'stale flat WSL resource layout' "${fixture}/error"

# A failed artifact build must leave the previous resource tree intact. A
# successful build must publish only the fully staged canonical layout.
fake_root="${fixture}/repo"
fake_bin="${fixture}/bin"
mkdir -p "${fake_root}/scripts/wsl" "${fake_root}/initramfs-patch/patch-rootfs.d" \
  "${fake_root}/build/wsl-resources" "${fake_bin}"
cp "${ROOT_DIR}/scripts/wsl/build-resources.sh" \
  "${ROOT_DIR}/scripts/wsl/resource-layout.sh" "${fake_root}/scripts/wsl/"
touch "${fake_root}/initramfs-patch/patch-rootfs.sh"
touch "${fake_root}/build/wsl-resources/sentinel"
cat >"${fake_bin}/docker" <<'FAKE_DOCKER'
#!/usr/bin/env bash
set -euo pipefail
[[ "${CDJ3K_FAKE_BUILD_FAIL:-0}" != 1 ]] || exit 17
for arg in "$@"; do
  case "${arg}" in
    type=local,dest=*) dest=${arg#type=local,dest=} ;;
  esac
done
mkdir -p "${dest}/modules"
for file in Image ep122_shim.so subucom_forwarder_aarch64 \
  subucom_live_aarch64 cfgd_aarch64 dummy_drv.so \
  modules/subucom_virt.ko modules/virtio_snd.ko modules/udev_usb1.ko; do
  if [[ "${CDJ3K_FAKE_BUILD_MISSING:-0}" == 1 && "${file}" == cfgd_aarch64 ]]; then
    continue
  fi
  touch "${dest}/${file}"
done
FAKE_DOCKER
chmod +x "${fake_bin}/docker"
if PATH="${fake_bin}:${PATH}" CDJ3K_FAKE_BUILD_FAIL=1 \
  bash "${fake_root}/scripts/wsl/build-resources.sh" >/dev/null 2>&1; then
  echo "failed build unexpectedly succeeded" >&2
  exit 1
fi
test -f "${fake_root}/build/wsl-resources/sentinel"
if PATH="${fake_bin}:${PATH}" CDJ3K_FAKE_BUILD_MISSING=1 \
  bash "${fake_root}/scripts/wsl/build-resources.sh" >/dev/null 2>&1; then
  echo "incomplete artifact build unexpectedly succeeded" >&2
  exit 1
fi
test -f "${fake_root}/build/wsl-resources/sentinel"
PATH="${fake_bin}:${PATH}" bash "${fake_root}/scripts/wsl/build-resources.sh" >/dev/null
validate_resource_layout "${fake_root}/build/wsl-resources"
test ! -e "${fake_root}/build/wsl-resources/sentinel"
echo "WSL scripts syntax passed"
