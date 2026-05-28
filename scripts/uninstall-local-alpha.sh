#!/usr/bin/env bash
set -euo pipefail

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
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "${prefix}" ]]; then
  prefix="${home_dir}/.local"
fi

rm -f "${prefix}/bin/harnesskit"
rm -rf "${home_dir}/.claude/skills/harnesskit"
rm -rf "${home_dir}/plugins/harnesskit"
rm -rf "${home_dir}/.agents/plugins/plugins/harnesskit"

marketplace_path="${home_dir}/.agents/plugins/marketplace.json"
if [[ -f "${marketplace_path}" ]]; then
  MARKETPLACE_PATH="${marketplace_path}" python3 - <<'PY'
import json
import os
from pathlib import Path

path = Path(os.environ["MARKETPLACE_PATH"])
try:
    data = json.loads(path.read_text())
except json.JSONDecodeError as exc:
    raise SystemExit(f"invalid marketplace JSON at {path}: {exc}")

plugins = data.get("plugins")
if isinstance(plugins, list):
    next_plugins = [plugin for plugin in plugins if plugin.get("name") != "harnesskit"]
    if len(next_plugins) != len(plugins):
        backup = path.with_suffix(path.suffix + ".harnesskit.bak")
        backup.write_text(path.read_text())
        data["plugins"] = next_plugins
        path.write_text(json.dumps(data, indent=2) + "\n")
PY
fi

echo "Removed local HarnessKit CLI, Claude skill, and Codex local plugin package."
echo "Removed harnesskit entry from ${home_dir}/.agents/plugins/marketplace.json if it existed."
