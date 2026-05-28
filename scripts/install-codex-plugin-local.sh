#!/usr/bin/env bash
set -euo pipefail

script_dir="$(dirname "${BASH_SOURCE[0]}")"
if [[ -f "${PWD}/Cargo.toml" && -d "${PWD}/agent-surfaces" ]]; then
  repo_root="${PWD}"
else
  repo_root="$(cd "${script_dir}/.." && pwd)"
fi
home_dir="${HOME}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --home)
      home_dir="$2"
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

# For the default personal marketplace at ~/.agents/plugins/marketplace.json,
# Codex resolves source.path relative to $HOME, so ./plugins/harnesskit maps to
# ~/plugins/harnesskit.
plugin_root="${home_dir}/plugins/harnesskit"
marketplace_path="${home_dir}/.agents/plugins/marketplace.json"

if [[ ! -f "${repo_root}/agent-surfaces/codex/.codex-plugin/plugin.json" ]]; then
  echo "missing Codex plugin manifest under ${repo_root}/agent-surfaces/codex" >&2
  exit 1
fi
if [[ ! -f "${repo_root}/agent-surfaces/skills/harnesskit/SKILL.md" ]]; then
  echo "missing shared skill under ${repo_root}/agent-surfaces/skills/harnesskit" >&2
  exit 1
fi

rm -rf "${plugin_root}"
mkdir -p "${plugin_root}/.codex-plugin"
mkdir -p "${plugin_root}/skills/harnesskit"
cp "${repo_root}/agent-surfaces/codex/.codex-plugin/plugin.json" "${plugin_root}/.codex-plugin/plugin.json"
cp "${repo_root}/agent-surfaces/skills/harnesskit/SKILL.md" "${plugin_root}/skills/harnesskit/SKILL.md"

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

echo "Installed local Codex HarnessKit plugin to ${plugin_root}"
echo "Wrote personal marketplace to ${marketplace_path}"
echo
echo "Next Codex steps for this user:"
echo "  codex plugin marketplace add ${home_dir}/.agents/plugins"
echo "  codex plugin list"
echo "  codex plugin add harnesskit@personal"
echo "Then restart Codex so the HarnessKit skill is loaded."
