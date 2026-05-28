#!/usr/bin/env bash
set -euo pipefail

script_dir="$(dirname "${BASH_SOURCE[0]}")"
if [[ -d "${PWD}/agent-surfaces" && -d "${PWD}/scripts" ]]; then
  source_skill="agent-surfaces/skills/harnesskit/SKILL.md"
  codex_skill_dir="agent-surfaces/codex/skills/harnesskit"
else
  repo_root="$(cd "${script_dir}/.." && pwd)"
  source_skill="${repo_root}/agent-surfaces/skills/harnesskit/SKILL.md"
  codex_skill_dir="${repo_root}/agent-surfaces/codex/skills/harnesskit"
fi

mkdir -p "${codex_skill_dir}"
cp "${source_skill}" "${codex_skill_dir}/SKILL.md"

echo "Synced shared HarnessKit skill into Codex packaging."
