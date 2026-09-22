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

# Rootfs patch scripts must use in-place editing syntax accepted by GNU sed
# on WSL as well as BSD sed on macOS.
patch_root="${fixture}/patch-root"
mkdir -p "${patch_root}/home/root/scripts" "${patch_root}/etc"
cat >"${patch_root}/home/root/scripts/apl_start.sh" <<'EOF'
SN65REG=$(/sbin/i2cget -f -y 3 0x2c 0xe5)
/bin/aplay silence.wav
EOF
ROOTFS="${patch_root}" bash "${ROOT_DIR}/initramfs-patch/patch-rootfs.d/01-sn65-stub.sh" >/dev/null
grep -q 'SN65REG="0x00"' "${patch_root}/home/root/scripts/apl_start.sh"
grep -q 'QEMU: skip aplay' "${patch_root}/home/root/scripts/apl_start.sh"
test ! -e "${patch_root}/home/root/scripts/apl_start.sh.bak"
printf '%s\n' 'root:locked:1:2:3:4:5:6:7' >"${patch_root}/etc/shadow"
ROOTFS="${patch_root}" ENABLE_SSH=1 \
  bash "${ROOT_DIR}/initramfs-patch/patch-rootfs.d/04-root-password.sh" >/dev/null
grep -q '^root::' "${patch_root}/etc/shadow"
test ! -e "${patch_root}/etc/shadow.bak"

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

# Detached builds must survive their launcher, capture a file log, and expose
# a final status without requiring the caller to hold the build session open.
PATH="${fake_bin}:${PATH}" \
  bash "${fake_root}/scripts/wsl/build-resources.sh" --background >/dev/null
async_status="${fake_root}/build/wsl-resource-build/status"
for _ in {1..100}; do
  [[ -f "${async_status}" ]] || { sleep 0.05; continue; }
  [[ "$(sed -n 's/^state=//p' "${async_status}")" == running ]] || break
  sleep 0.05
done
PATH="${fake_bin}:${PATH}" \
  bash "${fake_root}/scripts/wsl/build-resources.sh" --status >/dev/null
grep -qx 'state=succeeded' "${async_status}"
grep -q 'WSL resources staged' "${fake_root}/build/wsl-resource-build/build.log"
echo "WSL scripts syntax passed"
