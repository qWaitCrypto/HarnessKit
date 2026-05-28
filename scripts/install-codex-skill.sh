#!/usr/bin/env bash
set -euo pipefail

script_dir="$(dirname "${BASH_SOURCE[0]}")"
if [[ -f "${PWD}/Cargo.toml" && -d "${PWD}/agent-surfaces" ]]; then
  repo_root="${PWD}"
else
  repo_root="$(cd "${script_dir}/.." && pwd)"
fi
home_dir="${HOME}"
codex_home="${CODEX_HOME:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --home)
      home_dir="$2"
      shift 2
      ;;
    --codex-home)
      codex_home="$2"
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

if [[ -z "${codex_home}" ]]; then
  codex_home="${home_dir}/.codex"
fi

source_dir="${repo_root}/agent-surfaces/skills/harnesskit"
target_dir="${codex_home}/skills/harnesskit"

if [[ ! -f "${source_dir}/SKILL.md" ]]; then
  echo "missing shared skill at ${source_dir}/SKILL.md" >&2
  exit 1
fi

mkdir -p "${target_dir}"
cp "${source_dir}/SKILL.md" "${target_dir}/SKILL.md"

echo "Installed Codex HarnessKit skill to ${target_dir}"
