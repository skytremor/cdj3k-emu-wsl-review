#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT_DIR="${ROOT_DIR}/build/wsl-resources"
SCRIPT_DIR="${ROOT_DIR}/scripts/wsl"
SCRIPT_PATH="${SCRIPT_DIR}/build-resources.sh"
ASYNC_DIR="${ROOT_DIR}/build/wsl-resource-build"
ASYNC_LOG="${ASYNC_DIR}/build.log"
ASYNC_STATUS="${ASYNC_DIR}/status"
ASYNC_PID="${ASYNC_DIR}/pid"
ASYNC_HEAD="${CDJ3K_ASYNC_HEAD:-$(git -C "${ROOT_DIR}" rev-parse HEAD 2>/dev/null || printf unknown)}"
source "${SCRIPT_DIR}/resource-layout.sh"

write_async_status() {
  local state=${1:?state required}
  local tmp="${ASYNC_STATUS}.tmp.$$"
  {
    printf 'state=%s\n' "${state}"
    printf 'head=%s\n' "${ASYNC_HEAD}"
    printf 'updated_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'log=%s\n' "${ASYNC_LOG}"
  } >"${tmp}"
  mv "${tmp}" "${ASYNC_STATUS}"
}

case "${1:-}" in
  --background)
    mkdir -p "${ASYNC_DIR}"
    if [[ -f "${ASYNC_STATUS}" && -f "${ASYNC_PID}" ]] &&
       grep -qx 'state=running' "${ASYNC_STATUS}" &&
       kill -0 "$(cat "${ASYNC_PID}")" 2>/dev/null; then
      echo "WSL resource build is already running (pid $(cat "${ASYNC_PID}"))" >&2
      exit 1
    fi
    : >"${ASYNC_LOG}"
    write_async_status running
    CDJ3K_ASYNC_HEAD="${ASYNC_HEAD}" \
      nohup "${SCRIPT_PATH}" --worker >"${ASYNC_LOG}" 2>&1 </dev/null &
    worker_pid=$!
    printf '%s\n' "${worker_pid}" >"${ASYNC_PID}"
    echo "WSL resource build started in background (pid ${worker_pid})"
    echo "Log: ${ASYNC_LOG}"
    echo "Check once after completion: ${SCRIPT_PATH} --status"
    exit 0
    ;;
  --status)
    if [[ ! -f "${ASYNC_STATUS}" ]]; then
      echo "No detached WSL resource build has been started" >&2
      exit 1
    fi
    cat "${ASYNC_STATUS}"
    case "$(sed -n 's/^state=//p' "${ASYNC_STATUS}")" in
      succeeded) exit 0 ;;
      running)
        if [[ -f "${ASYNC_PID}" ]] && kill -0 "$(cat "${ASYNC_PID}")" 2>/dev/null; then
          exit 3
        fi
        echo "detached build exited without recording a result; inspect ${ASYNC_LOG}" >&2
        exit 1
        ;;
      *) exit 1 ;;
    esac
    ;;
  --worker)
    ASYNC_WORKER=1
    ;;
  "")
    ASYNC_WORKER=0
    ;;
  *)
    echo "usage: ${SCRIPT_PATH} [--background|--status]" >&2
    exit 2
    ;;
esac

mkdir -p "${ROOT_DIR}/build"
STAGE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/cdj3k-wsl-resources.XXXXXX")"
NEXT_DIR="$(mktemp -d "${ROOT_DIR}/build/.wsl-resources.next.XXXXXX")"
OLD_DIR="${NEXT_DIR}.previous"
cleanup() {
  local result=$?
  if [[ -e "${OLD_DIR}" && ! -e "${OUT_DIR}" ]]; then
    mv "${OLD_DIR}" "${OUT_DIR}"
  fi
  rm -rf "${STAGE_DIR}" "${NEXT_DIR}" "${OLD_DIR}"
  if (( ASYNC_WORKER )); then
    if (( result == 0 )); then
      write_async_status succeeded
    else
      write_async_status "failed:${result}"
    fi
  fi
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
