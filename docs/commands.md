# Commands

Commands below use HarnessKit 0.1.0-alpha.7. From a source checkout, replace
`harnesskit` with `cargo run --`, or build once and use `target/release/harnesskit`.
Use each command's `--help` for its complete option list.

## Target and argument order

Project commands default to the current directory and accept `--schema <path>`
and `--docs-root <path>`. Input precedes the optional target directory:

```text
harnesskit query <terms> [target_dir] [--limit N] [--json]
harnesskit context <terms-or-path> [target_dir] [--json]
harnesskit context --current [target_dir] [--json]
harnesskit context --since <snapshot|latest|YYYY-MM-DD> [target_dir] [--json]
harnesskit check [target_dir] [--format summary|full|json] [--all]
harnesskit index [target_dir] [--json]
harnesskit list-docs [target_dir] [--json]
harnesskit inspect <doc_path> [target_dir]
harnesskit refs <doc_path> [target_dir]
harnesskit graph <doc_path> [target_dir] [--json]
harnesskit rank [target_dir]
```

Quote multiword query terms. `query "auth session" /path/to/repo --limit 5`
searches that repository; `query "auth session" 5` treats `5` as a target and
fails with exit 2 if it does not exist. Only `init` may create a missing target.
In `context --current`/`--since` mode the sole positional is the target; bare
`context` returns current state for `.`.

## Common workflow

```bash
# Run from the project root after reading its agent entrypoint.
harnesskit context --current --json
harnesskit query "auth session" --limit 5 --json
harnesskit context docs/design-docs/auth.md --json
# After editing documentation, check refreshes the index itself.
harnesskit check --json
harnesskit history snapshot -m "Document the auth boundary"
harnesskit context --since latest --json
```

These auth paths belong to the bundled example. From the HarnessKit checkout:

```bash
harnesskit query "auth session" examples/web-app-with-auth --json
harnesskit context --current examples/web-app-with-auth --json
```

Search uses strict and relaxed matches, defaults to eight candidates, and marks
historical evidence. Read `coverage`, `hints` and candidate `match` fields. Zero
results exit 0 with diagnostics and title suggestions; change keywords before
falling back to a source search. Explicit `index` is useful for inspecting index
statistics; it is unnecessary before query, context or check.

## Checks and baselines

```bash
harnesskit check                         # Rule counts and baseline differences
harnesskit check --all                   # Full text
harnesskit check --format full           # Same full text mode
harnesskit check --json                   # Bounded structured report
harnesskit check --json --all             # Every finding and list entry
harnesskit check --strict                 # Warnings also cause exit 1
harnesskit check --rules broken-link --json
harnesskit check --update-baseline        # Review differences, then accept them
```

Choose either `--json` or `--format json`. The first evaluated check creates
`.harnesskit/state/check-baseline.json`; subsequent checks compare with it until
explicitly updated. Baselines do not suppress failures. Truncated JSON lists
report omitted counts; exit status still considers all selected findings.
Findings include repair hints; `fixable: false` means no automated repair exists.
A clean docs check does not validate application behavior or delivery.

## Layout, diagnostics and initialization

```bash
harnesskit doctor --layout --json
harnesskit doctor --code --json                  # Language, file and symbol counts
harnesskit doctor --layout --write-schema --preview
harnesskit doctor --layout --write-schema
harnesskit doctor --layout --write-schema --force  # Replace an existing schema
harnesskit init /path/to/new-project --preview
harnesskit init /path/to/new-project --tracked     # Team-visible docs
harnesskit init --local                           # Local Git excludes if in Git
```

Preview never writes project files. Ordinary doctor explains layout without indexing;
`--write-schema` previews an existing schema unless `--force` is given. Review
inferred coverage before saving it. Ordinary commands show inferred-layout
explanations once; `--verbose` repeats them. Doctor also checks installed skill
versions and provides release update or source sync instructions.

Source builds implementing Weave W1 refresh code nodes during indexing and with
`doctor --code`. Rust, TypeScript/TSX, JavaScript/JSX, Python and Go use tree-sitter
symbol queries; other UTF-8 text files retain file nodes, while binary files and
Markdown are excluded from the code scan. Git projects scan tracked files only,
respecting Git ignores and schema `ignore`; other directories use a walk honoring
`.gitignore` and `.ignore`. File states reuse unchanged mtime/size entries. Supported
languages fingerprint comment-free, whitespace-free token streams with literals
preserved; Python retains semantic INDENT/DEDENT boundaries and ignores explicit
line continuations. Unsupported files use content hashes. Node spans are inclusive
source line numbers starting at one. Existing document index output remains unchanged.
If Git cannot inspect an existing worktree, its cached code nodes are preserved;
`doctor --code` reports that the counts may be stale instead of scanning untracked files.

Coverage and `unmanaged-docs` use the same Markdown paths, including existing
Markdown referenced by managed documents outside the scan root. Unmanaged files
stay outside the index and history until mapped. Repair hints preserve multiline
YAML and indentation in summary, full text and JSON output.

The source repository's `.harnesskit/schema.yaml` keeps `docs/` as its scan root,
maps `README.md` and `CHANGELOG.md` to notes, `ITERATION_PLAN.md` to execution
plans, and `agent-surfaces/**/*.md` to notes. Run `cargo run -- check --strict`
to check this layout. Git versions these source documents; the schema suppresses
only `history-missing` because local HarnessKit snapshots are optional here.

## History

```text
harnesskit history status [target_dir] [--json]
harnesskit history snapshot [target_dir] -m <message> [--allow-empty] [--json]
harnesskit history list [target_dir] [--json]
harnesskit history diff <snapshot|latest> [target_dir] [--json]
harnesskit history diff <snapshot-a> <snapshot-b> [target_dir] [--json]
harnesskit history restore <snapshot|latest> [target_dir] [--apply] [--force] [--json]
harnesskit history show <snapshot|latest> <path> [target_dir] [--json]
harnesskit history export <file.tar.gz> [target_dir] [--json]
harnesskit history import <file.tar.gz> [target_dir] [--json]
```

Snapshot skips unchanged contents unless `--allow-empty` is used. Restore is a
preview without `--apply`. Import merges history without restoring documents or
replacing existing snapshot IDs; export refuses to overwrite a file. For an
ambiguous two-argument diff, append `.` as the explicit third argument to select
two snapshots in the current repository. Keep `.harnesskit/history`; state alone
is disposable. See the [migration guide](migration-alpha.7.md).

## Local usage

Logging is on by default, local only, under `~/.harnesskit/usage.jsonl` or
`$HARNESSKIT_HOME/usage.jsonl`. Minimal logs omit query text and raw paths.

```bash
export HARNESSKIT_USAGE_LOG=off          # Disable future writes
harnesskit doctor --usage --days 30 --json
# HARNESSKIT_USAGE_LOG=full opts into query text; unset for minimal logging.
```

## Source validation

Run from the HarnessKit source root with Rust/Cargo and a C compiler installed:

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings  # Requires the Clippy component
cargo build --release
```

Smoke tests must copy `tests/fixtures/{standard,flat,cjk,lifecycle,external}` to
temporary directories before running the release binary's index, check,
`context --current` and query commands. Preserve source fixtures, especially
the frozen old-schema/history compatibility assets. Exit codes: 0 normal,
1 documentation errors (or strict warnings), 2 usage/environment errors;
closing an output pipe exits silently with 0.
