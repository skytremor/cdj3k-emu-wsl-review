#!/usr/bin/env bash

version_at_least_1_85() {
  local version=${1:?version required}
  [[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]] || return 1
  local major=${version%%.*}
  local rest=${version#*.}
  local minor=${rest%%.*}
  (( major > 1 || (major == 1 && minor >= 85) ))
}

require_rust_toolchain() {
  local tool output version
  for tool in cargo rustc; do
    if ! command -v "${tool}" >/dev/null 2>&1; then
      echo "missing ${tool}; install Rust 1.85 or newer and select it on PATH" >&2
      return 1
    fi
    output=$("${tool}" --version) || return 1
    version=${output#"${tool} "}
    version=${version%% *}
    if ! version_at_least_1_85 "${version}"; then
      echo "${tool} ${version} is too old; select Rust/Cargo 1.85 or newer on PATH" >&2
      return 1
    fi
  done
}
