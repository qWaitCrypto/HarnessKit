# Migration to 0.1.0-alpha.7

Upgrade the CLI and the whole shared skill directory, including `references/`.
For a release install, use `harnesskit update --version v0.1.0-alpha.7` once that
release is available. For a source checkout, use `scripts/install-local-cli.sh`
and the relevant `scripts/install-codex-skill.sh` or
`scripts/install-claude-skill.sh`. `harnesskit doctor --json` compares installed
skill metadata against the bundled version and suggests how to sync differences.

## Existing files and history

- Keep `.harnesskit/history` intact. Old snapshots remain usable with
  `history list`, `history diff` and `history restore`; readers do not require
  the coverage metadata added to new snapshots. `history show` reads a single
  saved file, and export/import can transfer snapshots between machines.
- `.harnesskit/state` is derived. Indexed commands automatically rebuild an
  incompatible `facts.sqlite` from source documents without rewriting history
  or the project schema. A rebuild resets external-reference hash baselines;
  missing/incompatible check baselines are recreated on the next check.
- Existing `.harnesskit/schema.yaml` files need no edits. `schema_version` stays
  **0**, independent of the CLI version. Explicit schemas retain their required
  entrypoints/core docs/index checks. Zero-config inferred layouts do not require
  the entire init scaffold. Do not run `init --force` to upgrade existing docs.
- Discovery now includes flat Markdown notes, aliases such as `reference/`,
  `adr/`, `plans/`, `design/`, `specs/`, `runbooks/`, and filename-based roles.
  When no schema/docs-root is explicit and `docs/` is absent, it scans the repo
  root. Hidden trees, templates, dependencies/build output, symlinks, nested
  projects and fixture trees are excluded by default. Scope and snapshots may
  grow; review `harnesskit doctor --layout --json` without relocating documents.

## Optional schema and document fields

Add only the overrides your layout needs to the existing schema:

```yaml
include:
  "docs/misc/**/*.md": notes
  "research/*.md": references
ignore:
  - "docs/private/**"
```

Globs are repository-relative; `*` stays within one directory and `**` spans
directories. Existing collection mappings outrank inference, the first matching
include wins, and ignore wins over include. Includes can opt nested/fixture or
out-of-root Markdown into management. Supporting notes need no collection index.
External linked evidence is hashed for change detection, but is not snapshotted
unless it is explicitly managed Markdown.

Coverage also counts existing Markdown referenced by managed documents outside
the scan root. These paths share classification and directory counts with
`unmanaged-docs`; they remain unmanaged until an include/core/entrypoint mapping
applies. Templates, hidden/build/dependency trees and symlinks remain skipped,
and default exclusions and ignores still apply. Check hints retain pasteable
multiline YAML in the default summary as well as full text and JSON.

New-style suppressions accept path globs; existing named `rule_id` suppressions
keep exact-path matching, including literal glob characters:

```yaml
suppressions:
  - rule: canonical-links-reference
    path: 'docs/design-docs/**/*.md'
    reason: 'These links deliberately cite supporting evidence.'
```

Optional `rules.entrypoint_max_lines` (default 120) and
`rules.stale_active_plan_days` (default 30) set lifecycle check thresholds; add
them to the schema's existing `rules` block. Frontmatter now accepts
`superseded_by` and `supersedes_parts_of`; execution plans and worklogs accept
`status`, including with older schemas. Explicit status takes precedence for the
reported status, followed by path, a leading banner and the collection default.
Historical paths/banners and supersession still mark `lifecycle: historical`
even when frontmatter says active; check reports the conflict.

`doctor --layout --write-schema` saves inferred mappings. If a schema already
exists, it previews the proposal; `--force` permits replacement. Use `--preview`
to prevent all project writes. Installing a release leaves existing entrypoints
and project maps untouched; adopt dated current-focus/history sections manually
where useful and keep current state only in the project map.

## Update scripts for changed behavior

| Behavior | Script adjustment |
| --- | --- |
| Exit 0 = normal; 1 = docs errors or strict warnings; 2 = usage/environment failure. | Handle each status. A baseline or truncated output does not make errors pass. Malformed schemas produce exit 1 in check, exit 2 elsewhere. |
| Missing targets are no longer created by read/check commands. | Verify the target exists; use init when creation is intended. Query order is `query <terms> [target_dir]`; use `--limit N` instead of a positional count. |
| Check defaults to a summary with new/resolved baseline differences. | Use `--all`/`--format full` for all text, or `--json`/`--format json` for machine output. Do not combine the two JSON selectors. |
| JSON check lists are truncated by default, with complete counts. | Read truncation counters, or request `--json --all`. First check creates the baseline; `--update-baseline` explicitly replaces it after showing differences. |
| Search returns up to 8 candidates and may relax multiword matching. | Set `--limit`, inspect `match.matched_terms`, `missing_terms`, `mode` and `lifecycle`. Zero hits still exit 0; use diagnostics and hints to change keywords. |
| Bare context returns current state. | Use `context --current [target_dir]`; focused mode remains `context <terms-or-path> [target_dir]`. Use `--since` with a snapshot for content changes or a UTC date for mtime-based handoffs. |
| Identical snapshots are skipped. | Use `history snapshot --allow-empty` if a new ID is required. Non-Git `host_git_dirty` is null; history status supplies age/change/unmanaged/Git-tracking fields. |
| Closed pipes exit 0 without panic. | Pipelines such as `harnesskit list-docs \| head` no longer signal a broken-pipe failure. |

Local usage logging is **enabled by default**. Set
`export HARNESSKIT_USAGE_LOG=off` to disable writes; this does not delete old
records. Logs live at `~/.harnesskit/usage.jsonl`, overridden by
`HARNESSKIT_HOME`. Default records contain command/timing/outcome metrics and a
project-root hash, with no raw paths, contents or query text. Set
`HARNESSKIT_USAGE_LOG=full` only to opt into query text. Logs stay local;
`doctor --usage --days 30` summarizes them without uploading anything.

The runtime binary bundles SQLite; no system `sqlite3` is required. Building
from source needs Rust/Cargo and a C compiler. Query, context and check refresh
the index themselves: remove repeated `--version` and `index && check` rituals.
Use one check after doc edits, and snapshot at phase boundaries. Docs health is
separate from project tests, runtime validation and owner acceptance.
