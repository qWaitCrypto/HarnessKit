#!/usr/bin/env bash
set -euo pipefail

repo="qWaitCrypto/HarnessKit"
version="latest"
prefix="${HOME}/.local"
install_claude=1
install_codex=1
install_codex_plugin=0
tmp_dir=""
asset_base_url=""

usage() {
  cat <<'EOF'
Install HarnessKit.

Usage:
  install.sh [options]

Options:
  --version <tag>       Release tag to install, e.g. v0.1.0-alpha.3 (default: latest)
  --prefix <path>       Install prefix for the CLI (default: ~/.local)
  --repo <owner/name>   GitHub repository (default: qWaitCrypto/HarnessKit)
  --asset-base-url <url>
                        Override release asset base URL, mainly for testing
  --cli-only            Install only the harnesskit CLI
  --no-claude           Do not install Claude Code skill
  --no-codex            Do not install Codex skill
  --with-codex-plugin   Also install optional Codex local plugin package
  -h, --help            Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      if [[ $# -lt 2 || "${2:-}" == -* ]]; then
        echo "--version requires a release tag, e.g. v0.1.0-alpha.3" >&2
        exit 2
      fi
      version="$2"
      shift 2
      ;;
    --prefix)
      if [[ $# -lt 2 || "${2:-}" == -* ]]; then
        echo "--prefix requires a path" >&2
        exit 2
      fi
      prefix="$2"
      shift 2
      ;;
    --repo)
      if [[ $# -lt 2 || "${2:-}" == -* ]]; then
        echo "--repo requires owner/name" >&2
        exit 2
      fi
      repo="$2"
      shift 2
      ;;
    --asset-base-url)
      if [[ $# -lt 2 || "${2:-}" == -* ]]; then
        echo "--asset-base-url requires a URL" >&2
        exit 2
      fi
      asset_base_url="${2%/}"
      shift 2
      ;;
    --cli-only)
      install_claude=0
      install_codex=0
      install_codex_plugin=0
      shift
      ;;
    --no-claude)
      install_claude=0
      shift
      ;;
    --no-codex)
      install_codex=0
      install_codex_plugin=0
      shift
      ;;
    --with-codex-plugin)
      install_codex_plugin=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "${os}:${arch}" in
    Linux:x86_64|Linux:amd64)
      echo "x86_64-unknown-linux-gnu"
      ;;
    *)
      echo "unsupported platform: ${os} ${arch}" >&2
      echo "This alpha installer currently supports Linux x86_64 only." >&2
      exit 1
      ;;
  esac
}

download() {
  local url="$1"
  local out="$2"
  if command -v curl >/dev/null 2>&1; then
    if ! curl -fsSL --retry 3 --retry-delay 2 --connect-timeout 20 --max-time 120 "$url" -o "$out"; then
      echo "download failed: ${url}" >&2
      echo "If you are behind a proxy, set http_proxy, https_proxy, or all_proxy and retry." >&2
      exit 1
    fi
  elif command -v wget >/dev/null 2>&1; then
    if ! wget -q --tries=3 --timeout=120 "$url" -O "$out"; then
      echo "download failed: ${url}" >&2
      echo "If you are behind a proxy, set http_proxy, https_proxy, or all_proxy and retry." >&2
      exit 1
    fi
  else
    echo "missing required command: curl or wget" >&2
    exit 1
  fi
}

upsert_codex_marketplace() {
  local marketplace_path="$1"
  mkdir -p "$(dirname "${marketplace_path}")"
  MARKETPLACE_PATH="${marketplace_path}" python3 - <<'PY'
import json
import os
from pathlib import Path

path = Path(os.environ["MARKETPLACE_PATH"])
entry = {
    "name": "harnesskit",
    "source": {"source": "local", "path": "./plugins/harnesskit"},
    "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
    "category": "Coding",
}

if path.exists():
    try:
        data = json.loads(path.read_text())
    except json.JSONDecodeError as exc:
        raise SystemExit(f"invalid marketplace JSON at {path}: {exc}")
    backup = path.with_suffix(path.suffix + ".harnesskit.bak")
    backup.write_text(path.read_text())
else:
    data = {
        "name": "personal",
        "interface": {"displayName": "Personal"},
        "plugins": [],
    }

plugins = data.setdefault("plugins", [])
plugins[:] = [plugin for plugin in plugins if plugin.get("name") != "harnesskit"]
plugins.append(entry)
path.write_text(json.dumps(data, indent=2) + "\n")
PY
}

need_cmd tar
need_cmd sha256sum
if [[ "${install_codex_plugin}" -eq 1 ]]; then
  need_cmd python3
fi

case "${repo}" in
  */*) ;;
  *)
    echo "--repo must use owner/name format" >&2
    exit 2
    ;;
esac

target="$(detect_target)"
package="harnesskit-${target}"
archive="${package}.tar.gz"

if [[ -z "${asset_base_url}" ]]; then
  if [[ "${version}" == "latest" ]]; then
    asset_base_url="https://github.com/${repo}/releases/latest/download"
  else
    asset_base_url="https://github.com/${repo}/releases/download/${version}"
  fi
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

echo "Downloading HarnessKit ${version} for ${target}..."
echo "Install prefix: ${prefix}"
if [[ "${install_claude}" -eq 1 ]]; then
  echo "Claude skill: ${HOME}/.claude/skills/harnesskit/SKILL.md"
fi
if [[ "${install_codex}" -eq 1 ]]; then
  codex_home="${CODEX_HOME:-${HOME}/.codex}"
  echo "Codex skill: ${codex_home}/skills/harnesskit/SKILL.md"
fi
if [[ "${install_codex_plugin}" -eq 1 ]]; then
  echo "Optional Codex plugin package: ${HOME}/plugins/harnesskit"
fi
download "${asset_base_url}/${archive}" "${tmp_dir}/${archive}"
download "${asset_base_url}/SHA256SUMS" "${tmp_dir}/SHA256SUMS"

(
  cd "${tmp_dir}"
  sha256sum -c SHA256SUMS
)

tar -xzf "${tmp_dir}/${archive}" -C "${tmp_dir}"
package_dir="${tmp_dir}/${package}"

mkdir -p "${prefix}/bin"
cp "${package_dir}/harnesskit" "${prefix}/bin/harnesskit"
chmod +x "${prefix}/bin/harnesskit" 2>/dev/null || true
echo "Installed CLI: ${prefix}/bin/harnesskit"

if [[ "${install_claude}" -eq 1 ]]; then
  mkdir -p "${HOME}/.claude/skills/harnesskit"
  cp "${package_dir}/agent-surfaces/skills/harnesskit/SKILL.md" "${HOME}/.claude/skills/harnesskit/SKILL.md"
  echo "Installed Claude Code skill: ${HOME}/.claude/skills/harnesskit/SKILL.md"
fi

if [[ "${install_codex}" -eq 1 ]]; then
  codex_home="${CODEX_HOME:-${HOME}/.codex}"
  mkdir -p "${codex_home}/skills/harnesskit"
  cp "${package_dir}/agent-surfaces/skills/harnesskit/SKILL.md" "${codex_home}/skills/harnesskit/SKILL.md"
  echo "Installed Codex skill: ${codex_home}/skills/harnesskit/SKILL.md"
fi

if [[ "${install_codex_plugin}" -eq 1 ]]; then
  mkdir -p "${HOME}/plugins/harnesskit/.codex-plugin"
  mkdir -p "${HOME}/plugins/harnesskit/skills/harnesskit"
  cp "${package_dir}/agent-surfaces/codex/.codex-plugin/plugin.json" "${HOME}/plugins/harnesskit/.codex-plugin/plugin.json"
  cp "${package_dir}/agent-surfaces/skills/harnesskit/SKILL.md" "${HOME}/plugins/harnesskit/skills/harnesskit/SKILL.md"
  upsert_codex_marketplace "${HOME}/.agents/plugins/marketplace.json"
  echo "Installed Codex local plugin package: ${HOME}/plugins/harnesskit"
  echo "Updated Codex personal marketplace: ${HOME}/.agents/plugins/marketplace.json"
fi

echo
if [[ ":${PATH}:" != *":${prefix}/bin:"* ]]; then
  echo "Add HarnessKit to PATH for this shell:"
  echo "  export PATH=\"${prefix}/bin:\$PATH\""
  echo
fi
echo "HarnessKit installed successfully."
echo
echo "Verify:"
echo "  ${prefix}/bin/harnesskit --version"
echo "  ${prefix}/bin/harnesskit doctor"
echo
echo "Next in a repository, ask your coding agent:"
echo "  Initialize HarnessKit project docs for this repository."
echo
echo "Manual fallback:"
echo "  harnesskit init"
echo "  harnesskit index"
echo "  harnesskit check"
if [[ "${install_codex_plugin}" -eq 1 ]]; then
  echo
  echo "For optional Codex plugin mode, install the plugin after this script:"
  echo "  codex plugin list | grep harnesskit"
  echo "  codex plugin add harnesskit@personal"
fi
