#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
QEMU_COMMIT="f8ed81651e61d9c2166df6121ce2af0f44f06b3e" # v10.2.2
QEMU_SOURCE="${ROOT_DIR}/build/wsl-qemu-${QEMU_COMMIT:0:12}"
QEMU_PREFIX="${ROOT_DIR}/build/wsl-qemu-install"
QEMU_REPO="https://gitlab.com/qemu-project/qemu.git"

patch_dir="${ROOT_DIR}/qemu/patches/wsl"
patch_names=(
  wsl-04-shm-display-qapi-v10.2.2.patch
  wsl-05-shm-display-meson-v10.2.2.patch
  wsl-06-shm-display-source-v10.2.2.patch
)
patches=()
for name in "${patch_names[@]}"; do
  patch="${patch_dir}/${name}"
  [[ -f "${patch}" ]] || { echo "expected WSL QEMU patch missing: ${patch}" >&2; exit 1; }
  patches+=("${patch}")
done

if [[ "${1:-}" == "--list-patches" && $# -eq 1 ]]; then
  printf '%s\n' "${patches[@]}"
  exit 0
fi
if [[ $# -ne 0 ]]; then
  echo "Usage: $0 [--list-patches]" >&2
  exit 2
fi

mkdir -p "${ROOT_DIR}/build"
if [[ ! -d "${QEMU_SOURCE}/.git" ]]; then
  git clone "${QEMU_REPO}" "${QEMU_SOURCE}"
  git -C "${QEMU_SOURCE}" checkout --detach "${QEMU_COMMIT}"
else
  actual=$(git -C "${QEMU_SOURCE}" rev-parse HEAD)
  [[ "${actual}" == "${QEMU_COMMIT}" ]] || {
    echo "QEMU source is ${actual}, expected pinned ${QEMU_COMMIT}" >&2
    exit 1
  }
  if [[ -n "$(git -C "${QEMU_SOURCE}" status --porcelain)" ]]; then
    all_applied=true
    for patch in "${patches[@]}"; do
      git -C "${QEMU_SOURCE}" apply --reverse --check "${patch}" >/dev/null 2>&1 || all_applied=false
    done
    if [[ "${all_applied}" != true ]]; then
      echo "QEMU source has unrelated dirty changes; refusing to mix WSL patches" >&2
      exit 1
    fi
  fi
fi

for patch in "${patches[@]}"; do
  if git -C "${QEMU_SOURCE}" apply --reverse --check "${patch}" >/dev/null 2>&1; then
    continue
  fi
  git -C "${QEMU_SOURCE}" apply --check "${patch}"
  git -C "${QEMU_SOURCE}" apply "${patch}"
done

BUILD_DIR="${QEMU_SOURCE}/build"
if [[ ! -f "${BUILD_DIR}/build.ninja" ]]; then
  mkdir -p "${BUILD_DIR}"
  (
    cd "${BUILD_DIR}"
    ../configure \
      --prefix="${QEMU_PREFIX}" \
      --target-list=aarch64-softmmu \
      --disable-docs \
      --enable-tcg \
      -Ddefault_library=static \
      -Dslirp=enabled \
      -Dpa=enabled \
      -Dgtk=disabled \
      -Dsdl=disabled
  )
fi

MESON="${BUILD_DIR}/pyvenv/bin/meson"
[[ -x "${MESON}" ]] || { echo "QEMU configure did not create its Meson environment: ${MESON}" >&2; exit 1; }
"${MESON}" compile -C "${BUILD_DIR}" qemu-system-aarch64 qemu-img
"${MESON}" install -C "${BUILD_DIR}"
QEMU_BIN="${QEMU_PREFIX}/bin/qemu-system-aarch64"
"${QEMU_BIN}" -display help | grep -E '^[[:space:]]*shm([[:space:]]|$)' >/dev/null
echo "QEMU v10.2.2 (${QEMU_COMMIT}) installed in ${QEMU_PREFIX}"
