#!/usr/bin/env bash
set -euo pipefail

script_dir="$(dirname "${BASH_SOURCE[0]}")"
if [[ -f "${PWD}/Cargo.toml" && -d "${PWD}/agent-surfaces" ]]; then
  repo_root="${PWD}"
else
  repo_root="$(cd "${script_dir}/.." && pwd)"
fi
home_dir="${HOME}"
prefix=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --home)
      home_dir="$2"
      shift 2
      ;;
    --prefix)
      prefix="$2"
      shift 2
      ;;
    --repo-root)
      repo_root="$2"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "${prefix}" ]]; then
  prefix="${home_dir}/.local"
fi

cargo_bin="${CARGO:-}"
if [[ -z "${cargo_bin}" ]]; then
  if [[ -x /usr/bin/cargo ]]; then
    cargo_bin="/usr/bin/cargo"
  else
    cargo_bin="cargo"
  fi
fi

target_dir="${CARGO_TARGET_DIR:-${home_dir}/.cache/harnesskit/cargo-target}"
target_dir="$(mkdir -p "${target_dir}" && cd "${target_dir}" && pwd)"

if [[ -x /usr/bin/rustc && -z "${RUSTC:-}" ]]; then
  CARGO_TARGET_DIR="${target_dir}" RUSTC=/usr/bin/rustc "${cargo_bin}" build --release --manifest-path "${repo_root}/Cargo.toml"
else
  CARGO_TARGET_DIR="${target_dir}" "${cargo_bin}" build --release --manifest-path "${repo_root}/Cargo.toml"
fi
mkdir -p "${prefix}/bin"
cp "${target_dir}/release/harnesskit" "${prefix}/bin/harnesskit"

echo "Installed harnesskit CLI to ${prefix}/bin/harnesskit"
if [[ ":${PATH}:" != *":${prefix}/bin:"* ]]; then
  echo "Note: ${prefix}/bin is not currently on PATH."
  echo "Add it for this shell with: export PATH=\"${prefix}/bin:\$PATH\""
fi
