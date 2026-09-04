#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
for script in build-qemu.sh build-resources.sh preflight.sh resource-layout.sh run.sh; do
  bash -n "${ROOT_DIR}/scripts/wsl/${script}"
done
source "${ROOT_DIR}/scripts/wsl/resource-layout.sh"
test "${#resource_files[@]}" -gt 0

fixture="$(mktemp -d "${TMPDIR:-/tmp}/cdj3k-resource-layout.XXXXXX")"
trap 'rm -rf "${fixture}"' EXIT
for relative in "${resource_files[@]}"; do
  mkdir -p "${fixture}/$(dirname "${relative}")"
  touch "${fixture}/${relative}"
done
validate_resource_layout "${fixture}"
rm "${fixture}/tools/cfgd"
if validate_resource_layout "${fixture}"; then
  echo "resource validator accepted an incomplete fixture" >&2
  exit 1
fi
echo "WSL scripts syntax passed"
