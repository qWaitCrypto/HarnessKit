#!/usr/bin/env bash
set -euo pipefail

script_dir="$(dirname "${BASH_SOURCE[0]}")"
if [[ -f "${PWD}/Cargo.toml" && -d "${PWD}/agent-surfaces" ]]; then
  repo_root="${PWD}"
else
  repo_root="$(cd "${script_dir}/.." && pwd)"
fi

target_triple="x86_64-unknown-linux-gnu"
version=""
dist_dir=""
use_target_dir=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      target_triple="$2"
      shift 2
      ;;
    --version)
      version="$2"
      shift 2
      ;;
    --repo-root)
      repo_root="$2"
      shift 2
      ;;
    --dist-dir)
      dist_dir="$2"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "${version}" ]]; then
  version="$(grep -m1 '^version = ' "${repo_root}/Cargo.toml" | sed -E 's/version = "([^"]+)"/\1/')"
fi

cargo_bin="${CARGO:-}"
if [[ -z "${cargo_bin}" ]]; then
  if [[ -x /usr/bin/cargo ]]; then
    cargo_bin="/usr/bin/cargo"
  else
    cargo_bin="cargo"
  fi
fi

target_dir="${CARGO_TARGET_DIR:-${HOME}/.cache/harnesskit/cargo-target}"
target_dir="$(mkdir -p "${target_dir}" && cd "${target_dir}" && pwd)"
if [[ -z "${dist_dir}" ]]; then
  dist_dir="${repo_root}/dist"
fi
mkdir -p "${dist_dir}"
dist_dir="$(cd "${dist_dir}" && pwd)"
package_name="harnesskit-${target_triple}"
package_dir="${dist_dir}/${package_name}"
archive="${dist_dir}/${package_name}.tar.gz"
sums_file="${dist_dir}/SHA256SUMS"

if [[ -x /usr/bin/rustc && -z "${RUSTC:-}" ]]; then
  if [[ "${target_triple}" == "x86_64-unknown-linux-gnu" ]]; then
    CARGO_TARGET_DIR="${target_dir}" RUSTC=/usr/bin/rustc "${cargo_bin}" build --release --manifest-path "${repo_root}/Cargo.toml"
  else
    use_target_dir=1
    CARGO_TARGET_DIR="${target_dir}" RUSTC=/usr/bin/rustc "${cargo_bin}" build --release --target "${target_triple}" --manifest-path "${repo_root}/Cargo.toml"
  fi
else
  if [[ "${target_triple}" == "x86_64-unknown-linux-gnu" ]]; then
    CARGO_TARGET_DIR="${target_dir}" "${cargo_bin}" build --release --manifest-path "${repo_root}/Cargo.toml"
  else
    use_target_dir=1
    CARGO_TARGET_DIR="${target_dir}" "${cargo_bin}" build --release --target "${target_triple}" --manifest-path "${repo_root}/Cargo.toml"
  fi
fi

binary_path="${target_dir}/release/harnesskit"
if [[ "${use_target_dir}" -eq 1 ]]; then
  binary_path="${target_dir}/${target_triple}/release/harnesskit"
fi

rm -rf "${package_dir}" "${archive}"
mkdir -p "${package_dir}"

cp "${binary_path}" "${package_dir}/harnesskit"
cp "${repo_root}/README.md" "${package_dir}/README.md"
cp "${repo_root}/LICENSE" "${package_dir}/LICENSE"
cp -R "${repo_root}/agent-surfaces" "${package_dir}/agent-surfaces"
cp -R "${repo_root}/scripts" "${package_dir}/scripts"
cp -R "${repo_root}/schemas" "${package_dir}/schemas"
cp -R "${repo_root}/templates" "${package_dir}/templates"

cat > "${package_dir}/VERSION" <<EOF
${version}
EOF

tar -czf "${archive}" -C "${dist_dir}" "${package_name}"
(
  cd "${dist_dir}"
  if [[ ! -f SHA256SUMS ]]; then
    : > SHA256SUMS
  fi
  if [[ -f SHA256SUMS.tmp ]]; then
    rm -f SHA256SUMS.tmp
  fi
  if [[ -f SHA256SUMS ]]; then
    grep -v "  ${package_name}.tar.gz\$" SHA256SUMS > SHA256SUMS.tmp || true
    mv SHA256SUMS.tmp SHA256SUMS
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${package_name}.tar.gz" >> SHA256SUMS
  else
    shasum -a 256 "${package_name}.tar.gz" >> SHA256SUMS
  fi
)

echo "Packaged ${archive}"
echo "Wrote ${sums_file}"
