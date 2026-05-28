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

if [[ -x /usr/bin/rustc && -z "${RUSTC:-}" ]]; then
  CARGO_TARGET_DIR="${target_dir}" RUSTC=/usr/bin/rustc "${cargo_bin}" build --release --manifest-path "${repo_root}/Cargo.toml"
else
  CARGO_TARGET_DIR="${target_dir}" "${cargo_bin}" build --release --manifest-path "${repo_root}/Cargo.toml"
fi

rm -rf "${package_dir}" "${archive}"
mkdir -p "${package_dir}"

cp "${target_dir}/release/harnesskit" "${package_dir}/harnesskit"
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
  sha256sum "${package_name}.tar.gz" > SHA256SUMS
)

echo "Packaged ${archive}"
echo "Wrote ${dist_dir}/SHA256SUMS"
