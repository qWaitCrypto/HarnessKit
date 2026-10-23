# Changelog

## Unreleased (W5b output contract)

- Query and context text now use KWIC excerpts, abnormal-only badges, merged
  symbols/chapters, a shared token budget and opaque paging cursors. Query sets
  are cached under `.harnesskit/state/query-cache/` and invalidated by index
  generation; JSON keeps the existing fields and adds `next_cursor` and
  `truncated` (use `--full` when verbose evidence is required).
- Default query/context output omits coverage and repeated hints; check keeps a
  single coverage line. Existing JSON evidence fields remain for compatibility;
  future cleanup of redundant legacy fields is tracked here for the 0.2.0
  migration notes.

## 0.2.0

### Added

- Weave connects managed document chapters to code files and symbols through
  explicit anchors, same-commit evidence, identifier mentions and durable
  `.harnesskit/knowledge.lock` seals.
- Mixed `query` bundles, code-aware `context --for`/`--diff`,
  `suggest-anchors`, migration planning/application and opt-in edit hooks are
  available through the existing CLI surface.
- The shared skill and CLI reference document the 0.2.0 workflow; a dedicated
  [migration guide](docs/migration-0.2.0.md) covers compatibility and upgrades.

### Changed

- Retrieval and rendering use bounded, signal-first bundles with relevance
  floors, merged symbols, KWIC excerpts, paging and expansion. Weak inferred
  associations are omitted when they fall below the documented threshold.
- `check --ack` re-seals reviewed anchors and `check --fix` repairs uniquely
  moved anchors. Hooks remain disabled unless explicitly installed.

### Compatibility and migration

- `schema_version: 0` and zero-config repositories continue to work. Run
  `harnesskit migrate --plan` to review optional schema, layout and lock work,
  then `migrate --apply` for the safe structured changes.
- Commit `.harnesskit/knowledge.lock` when using code anchors; keep derived
  `.harnesskit/state/` ignored. Existing history snapshots remain readable.

## 0.1.0-alpha.7

### Breaking and behavior changes

- Exit codes now distinguish normal completion (`0`), documentation errors or
  warnings under `--strict` (`1`), and usage/environment errors (`2`). Broken
  links, malformed frontmatter/schema, and missing all explicitly configured
  entrypoints can make `check` fail. Baselines do not suppress errors.
- Only `init` creates a missing target directory. Other commands return 2 and
  suggest checking the positional arguments. Query syntax remains
  `query <terms> [target_dir]`; a number after the terms is a target, so use
  `--limit N` to set the result count.
- `check` defaults to a rule summary and new/resolved findings against a saved
  baseline. Use `--all` or `--format full` for full text, and `--json` or
  `--format json` for structured output. JSON lists are bounded by default;
  `--json --all` expands them. Exit status considers every selected finding.
- Search defaults to eight results and adds partial matches when fewer than
  three documents match all terms. Read each candidate's `match` metadata;
  historical documents rank below current evidence with equal term coverage.
- Discovery and history now include inferred flat notes and common collection
  aliases. Snapshot scope can therefore grow without changing the schema.
  Nested projects and fixture trees are excluded by default; explicit includes
  can opt them in. Review `coverage` or `doctor --layout` after upgrading.
- Unchanged history snapshots are skipped; use `history snapshot --allow-empty`
  to force a checkpoint. Outside Git, `host_git_dirty` is now `null`, not `false`.
- **Local usage logging is enabled by default.** Records stay in
  `~/.harnesskit/usage.jsonl` (or `$HARNESSKIT_HOME/usage.jsonl`) and omit raw paths,
  document contents and queries. Set `HARNESSKIT_USAGE_LOG=off` to disable writes;
  `HARNESSKIT_USAGE_LOG=full` opts into recording query text. Nothing is uploaded.

### Added

- Adaptive Markdown coverage with managed, unmanaged and excluded counts,
  per-directory mappings, schema `include`/`ignore` globs, collection aliases,
  filename heuristics and repository-root fallback when `docs/` is absent.
  `doctor --layout --write-schema` can save inferred mappings; replacing an
  existing schema requires `--force`.
- Coverage and next-step hints in structured index/check/discovery, query,
  context/graph, doctor and history reports. Long inferred-layout explanations
  appear once; `--verbose` repeats them.
- Unicode and trigram retrieval, short CJK substring matching, weighted fields,
  matching-line snippets, up to five focused neighbors, zero-result diagnostics
  and similar-title suggestions. Results expose missing terms, lifecycle and
  superseding documents.
- `context --current` (also bare `context`) for entrypoints, the project map,
  active plans, recent decisions, latest snapshot changes and unchecked items.
  `context --since <snapshot|latest|YYYY-MM-DD>` supplies a handoff packet.
- Lifecycle inference from frontmatter, paths and leading banners, plus
  `superseded_by` and `supersedes_parts_of` metadata. Actionable checks identify
  long entrypoints, duplicated current pointers, active/historical conflicts,
  unmarked supersession and stale active plans.
- Findings include repair hints and a `fixable` flag. A derived check baseline
  tracks new/resolved findings; `--update-baseline` accepts the reviewed set.
  New-style suppressions accept rule/path globs and a reason.
- External reference hashes detect changed linked evidence without putting it
  in history. Doctor/check also identify possible orphan `.harnesskit` directories.
- `history show`, portable `history export`/`history import`, snapshot coverage,
  and status fields for snapshot age, changed/unmanaged documents and Git tracking.
- `doctor --usage [--days N]` summarizes local counts, zero-hit and relaxed-hit
  rates, invalid invocations and redundant command patterns.

### Changed

- SQLite is bundled via `rusqlite`; core commands need no system `sqlite3`
  executable or shared library. JSON uses Serde; CLI help is generated by clap.
  Source builds need Rust/Cargo and a C compiler for bundled SQLite.
- Closed output pipes exit silently with 0 instead of panicking. Non-project
  directories receive an initialization hint without requiring reinitialization.
- Stale-anchor checks use batched/cached Git commit times, skip decisions and
  historical documents, and merge scopes per document. Without Git they use
  mtimes at info severity. Tracked docs no longer require local exclude entries;
  local isolation honors both `.gitignore` and `.git/info/exclude`.
- Reference citations are informational and only Markdown links trigger them;
  orphan/unmanaged documents are grouped by directory. Dirty-history warnings
  require changed docs and a snapshot older than seven days, or over twenty
  changed files.
- Coverage and `unmanaged-docs` share discovery, including existing Markdown
  referenced outside the scan root. Check summaries preserve pasteable multiline
  YAML hints. This repository explicitly maps its root release/plan documents
  and agent documentation without moving them into `docs/`.
- The shared agent skill is at most 60 lines, with command details in
  `references/cli.md`. It starts handoffs with current context, skips repeated
  version/index rituals, and separates docs health from delivery validation.
  Doctor checks installed skill versions and suggests update/sync actions.
- Init templates keep entrypoints short and current state in `docs/project.md`,
  with dated current focus and ended/historical sections. Updating the CLI does
  not overwrite existing entrypoints or project docs.

### Compatibility and migration

- Existing `schema_version: 0` files work unchanged, including exact-path
  suppressions using `rule_id`. All new schema fields are optional.
- Incompatible derived `facts.sqlite` indexes rebuild automatically. Existing
  history objects and snapshots remain readable by list/diff/restore; new
  snapshot coverage metadata is optional when reading older snapshots.
- See [migration notes](docs/migration-alpha.7.md) and [commands](docs/commands.md)
  for script updates and copyable examples.
