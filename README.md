<div align="center">

# HarnessKit

**A lightweight, file-first project engineering harness for coding agents.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Built with Rust](https://img.shields.io/badge/Built%20with-Rust-dea584.svg)](https://www.rust-lang.org/)
[![Status: Alpha](https://img.shields.io/badge/Status-Alpha-orange.svg)]()


Your repo is the system of record.<br>
Your agent is the operator.<br>
HarnessKit is the bridge.

</div>

Source version: **0.1.0-alpha.7** · [Changelog](CHANGELOG.md) · [Migration guide](docs/migration-alpha.7.md) · [Commands](docs/commands.md)

---

## The Problem

Coding agents (Codex, Claude Code, Cursor...) already handle the **agent runtime** — model loops, tool execution, sandboxing, conversations. That layer is well-covered.

What's missing is the **project engineering layer**: structured documentation, design decision records, progressive context disclosure, docs health checks, and local history. Today, `AGENTS.md` and `CLAUDE.md` become dumping grounds. Project knowledge scatters across chat threads that get compacted, truncated, or lost.

Asking users to learn a new CLI to fix this defeats the purpose — it just shifts the burden.

## The Idea

HarnessKit is inspired by OpenAI's [harness engineering](https://openai.com/index/harness-engineering/) philosophy: the repository itself should be the source of truth for project knowledge. But we take it further:

> **Build the tools for agents. Let agents serve the users.**

Users install a skill. Their agent handles everything else — initializing docs, querying context, checking consistency, managing history. No CLI to memorize. No new workflow to learn.

```
┌─────────────────────────────────────────────────┐
│  User                                           │
│  "What was the design rationale for auth?"      │
│                                                 │
│  ┌───────────────────────────────────────────┐  │
│  │  Agent (Codex / Claude Code / ...)        │  │
│  │                                           │  │
│  │  ┌─────────┐  ┌────────────────────────┐  │  │
│  │  │  Skill  │→ │  harnesskit CLI        │  │  │
│  │  │         │  │  query / context /     │  │  │
│  │  │         │  │  check / history / ... │  │  │
│  │  └─────────┘  └───────────┬────────────┘  │  │
│  └───────────────────────────┼───────────────┘  │
│                              ↓                  │
│  ┌───────────────────────────────────────────┐  │
│  │  Repository (source of truth)             │  │
│  │  AGENTS.md · docs/ · .harnesskit/        │  │
│  └───────────────────────────────────────────┘  │
└─────────────────────────────────────────────────┘
```

## Design Principles

| Principle | What it means |
|:--|:--|
| **File-first** | Repository files are project memory. No external databases, no cloud services. Everything is plain files in the repo. |
| **SQLite is derived** | `.harnesskit/state` is built from docs and can be rebuilt. `.harnesskit/history` is local checkpoint state; deleting it discards local doc snapshots. |
| **Built for agents** | The CLI is a tool surface that agents call. Users talk to their agent in natural language; the agent picks the right command. |
| **Not a runtime** | HarnessKit does not run models, manage conversations, or execute tools. It's the engineering harness, not the agent harness. |
| **Lightweight** | Rust CLI with bundled SQLite, no daemons, no MCP servers, no background processes. No system `sqlite3` installation is needed. |

## Quick Start

**Step 1** — Install the CLI and agent skill:

```bash
# Public alpha install
curl -fsSL https://raw.githubusercontent.com/qWaitCrypto/HarnessKit/main/install.sh | bash
```

This installs the latest public release by default. Use `--version <tag>` only when you intentionally need a pinned release.

If `~/.local/bin` is not on your `PATH`, add it:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

For local development from this repository:

```bash
scripts/install-local-cli.sh
scripts/install-claude-skill.sh        # Claude Code
scripts/install-codex-skill.sh         # Codex
```

Alpha notes:

- The public binary release currently supports Linux x86_64 and macOS x86_64/arm64.
- The binary includes the default schema and templates, so `harnesskit init` works from ordinary project directories without a checked-out HarnessKit source tree.
- SQLite is compiled into the binary. Indexing, search, checks, context, and history work without `sqlite3` or a SQLite shared library. Building from source requires Rust/Cargo and a C compiler for bundled SQLite.
- Existing history snapshots and schemas remain compatible. `doctor` no longer probes or requires the system `sqlite3` command.
- The installer writes the Codex skill directly; Codex plugin packaging is optional.
- The installer can be rerun with a newer `--version` tag to upgrade.
- After alpha.2, `harnesskit update` can update the CLI and already-installed skills from GitHub releases.

**Step 2** — Ask your agent to set up project docs:

```
> Initialize project documentation for this repo.
```

The agent runs `harnesskit init`, reads the entrypoint, and starts managing your project docs. That's it.

By default, init uses **local mode**. In a git repository, generated docs are added to `.git/info/exclude` so a trial does not change `git status`. Outside a git repository, init still creates the docs scaffold and skips git exclude setup with an explicit message. For team-visible or open-source project memory, use `harnesskit init --tracked`. To inspect what would be written first, use `harnesskit init --preview`.

**Step 3** — Work naturally:

```
> What was the design rationale for the auth module?
> Create a design doc for the new payment flow.
> Are our docs consistent? Anything broken?
> Snapshot the current docs before I refactor.
```

The agent calls the CLI behind the scenes. Commands record local usage metrics by
default; `export HARNESSKIT_USAGE_LOG=off` disables logging. No metrics are uploaded.

## 30-Second Demo

For a handoff, start with the current state packet. It includes entrypoint routes,
the project map, active plans, recent decisions, snapshot changes and unchecked
acceptance items:

```bash
harnesskit context --current --json
```

Then request focused context for the task:

```bash
harnesskit context auth --json
```

Excerpt of the answer (JSON also includes `coverage` and `hints`):

```json
{
  "anchor": {"path": "docs/design-docs/auth.md"},
  "incoming": [
    {"src_path": "docs/decisions/0003-auth-boundary.md"}
  ],
  "outgoing": [
    {"dst_path": "ARCHITECTURE.md"},
    {"dst_path": "docs/product-specs/login.md"}
  ],
  "recommended_reading_order": [
    "ARCHITECTURE.md",
    "docs/product-specs/login.md",
    "docs/design-docs/auth.md",
    "docs/decisions/0003-auth-boundary.md"
  ]
}
```

The output is the smallest useful reading path for the task: architecture, product context, the focused design doc, and the decision record that constrains it.

## What Init Creates

```
your-repo/
├── AGENTS.md / CLAUDE.md        # Agent entrypoints (thin routing tables)
├── ARCHITECTURE.md              # High-level code map
├── docs/
│   ├── index.md                 # Docs manifest
│   ├── project.md               # Project map
│   ├── DOCUMENTATION_SYSTEM.md  # Documentation rules
│   ├── commands.md              # Command reference
│   ├── product-specs/           # Canonical product specifications
│   ├── design-docs/             # Canonical design documents
│   ├── decisions/               # Architectural decision records
│   ├── exec-plans/              # Execution plans (active / completed)
│   ├── worklog/                 # Work logs (active / archive)
│   ├── operations/              # Runbooks, operational docs
│   ├── generated/               # Auto-generated documentation
│   ├── references/              # External reference material
│   └── templates/               # Templates for new docs
└── .harnesskit/                 # Internal state
    ├── state/                   # Derived index/fact store; safe to rebuild
    └── history/                 # Local doc checkpoints; do not delete as cache
```

Agents navigate this through **progressive disclosure**: entrypoint → architecture → manifest → collection index → the smallest doc that answers the task. They never bulk-read everything.

<details>
<summary><b>CLI Reference</b> (for agents — or curious humans)</summary>

<br>

#### Initialization

```bash
harnesskit init [target_dir] [--schema <path>] [--docs-root <path>] [--force] [--local|--tracked] [--preview]
```

Creates the docs scaffold. Existing files are skipped unless `--force` is passed. Default `--local` mode adds managed paths to `.git/info/exclude` when the target is inside a git repository; non-git directories are initialized without git exclude entries. Use `--tracked` to leave generated docs visible to git status, or `--preview` to print the plan without writing files.

#### Indexing

```bash
harnesskit index [target_dir] [--json]
```

Refreshes the SQLite fact store from current docs. Incremental — only processes changed files. Query, context and check refresh it automatically; ordinary doc edits need one check, without a preceding index.

#### Search & Context

```bash
harnesskit query "<terms>" [target_dir] [--limit N] [--json]
harnesskit context --current [target_dir] [--json]
harnesskit context --since <snapshot|latest|YYYY-MM-DD> [target_dir] [--json]
harnesskit context "<terms-or-path>" [target_dir] [--json]
harnesskit graph <doc_path> [target_dir] [--json]
```

Put query terms before the optional target: `harnesskit query "auth session" /path/to/repo --json`.
Use 1–3 distinctive keywords; `--limit` defaults to 8. Search combines Unicode
full-text and trigram indexes, with a substring fallback for short CJK terms.
Fewer than three strict matches adds partial matches; each candidate reports
`match.matched_terms`, `missing_terms` and `mode`. Title and path matches rank
above headings, summaries and body text; historical evidence ranks below current
documents with equal term coverage. Results identify `lifecycle` and
`superseded_by`, show snippets only from matching lines, and limit neighbors to
five focused documents. Zero hits still exit 0 and include per-term diagnostics,
unmanaged-file matches, title suggestions and hints for changing the query.

Bare `context` is the same as `context --current`. With `--current` or `--since`,
the optional positional is the target; in focused mode, input comes first.
Snapshot handoffs compare content hashes, including additions, edits and removals.
Date handoffs use UTC mtimes and group additions under modified documents.

#### Inspection

```bash
harnesskit inspect <doc_path> [target_dir]    # Metadata, relations and checks
harnesskit refs <doc_path> [target_dir]       # Incoming references
```

#### Discovery

```bash
harnesskit list-docs [target_dir] [--json]    # Managed docs with metadata
harnesskit rank [target_dir]                  # Top 20 most-referenced docs
```

Discovery scans all Markdown under `managed_root`, plus configured core files,
entrypoints, explicit includes and existing Markdown referenced by managed docs.
Referenced Markdown outside the scan root is counted in coverage and
`unmanaged-docs` together; references alone do not make those files managed.
It skips templates, hidden directories (including `.harnesskit`),
symlinks, `target`, and `node_modules`. Flat documents become supporting `notes`;
`reference`, `adr`/`adrs`, `plans`, `design`, `specs`, and `runbooks` map to the
corresponding collection roles. Filenames containing `decision` or `plan`, or
starting with `architecture`, also supply a role. Existing collection definitions
take precedence over inference, and inferred notes require no collection index.

When no schema or `--docs-root` is supplied and the default `docs/` is absent,
discovery uses the repository root and treats otherwise unclassified Markdown as
supporting notes. Discovery excludes nested projects (subdirectories containing
`AGENTS.md`, `CLAUDE.md`, or `.harnesskit/`) by default, with reason
`nested-project`. It also excludes `tests/fixtures/`, `testdata/`, and directories
named `fixture` or `fixtures` under `examples/`, at any depth, with reason
`fixture-dir` (which takes precedence for fixtures that are also nested projects).
Other example documentation remains eligible. Explicit roots and existing schemas
keep their configured scan scope; `include` can opt files back into management.

Optional schema fields override inference without moving files. Globs are relative
to the repository root; `*` matches within a directory, `**` spans directories.
The first matching include wins; ignore takes precedence over all mappings.
Includes override the default nested-project and fixture exclusions and can also
select Markdown outside `managed_root`.

```yaml
include:
  "docs/misc/**/*.md": notes
  "research/*.md": references
ignore:
  - "docs/private/**"
```

`index`, `check`, `list-docs`, and `history snapshot` print a coverage summary and
support `--json`. Coverage reports Markdown totals, managed/unmanaged/excluded
counts, directory counts by status, and every mapping with its reason. Its ratio
is `managed / (managed + unmanaged)` (0 for an empty scope); skipped trees are
outside the total, and excluded counts default exclusions and explicit ignore
matches. `hints` explain the scope and provide multiline YAML include snippets for
unmanaged directories, preserving indentation even in the default check summary.

`check` reports missing local link targets as `broken-link` errors, and existing
unmanaged Markdown as one `unmanaged-docs` warning per directory. Existing files
outside the docs root referenced by relative Markdown links or inline paths are
hashed in the derived `external_refs` table. An `external-ref-changed` info finding
names the referring document when those bytes change. It persists across commands
until that document is updated; rebuilding state resets the hash baseline. External
references are never added to history unless explicitly included as managed Markdown.

#### Health Check

```bash
harnesskit check [target_dir] [--format summary|full|json] [--all] [--update-baseline]
                [--json] [--strict] [--rules <rule-list>]
```

Validates all managed docs — missing required files, broken links, stale anchors, duplicate content, frontmatter issues, and more.

The default summary groups counts by rule and shows at most three current examples
and three new/resolved examples each. `--all` or `--format full` expands the text;
`--json` remains an alias for `--format json` (choose one, not both). Every finding includes a repair
`hint` and a `fixable` boolean. Automatic repairs are not currently implemented,
so `fixable` is false; suggested edits require review.

The first successful evaluation creates `.harnesskit/state/check-baseline.json`,
including when findings cause exit 1. Later checks report `baseline.new` and
`baseline.resolved` against that baseline without replacing it. Use
`--update-baseline` to accept the current findings **after** seeing their difference.
Finding identity uses rule, path, severity and, where needed, the linked target or
frontmatter field; wording and timestamps do not create new findings. Suppressed
findings are excluded from the baseline. `--rules` filters the report and its
differences, while baseline creation/updates always retain every active rule.
The baseline is derived data and is recreated if missing or incompatible.

JSON includes complete severity/rule counts. By default `checks` contains at most
20 findings and `truncated` counts omitted findings; other lists (including
coverage mappings, evidence paths and new/resolved entries) also stop at 20 with
`<list>_truncated` and a local `truncated` count. `--json --all` returns every list
in full. Exit status always uses all selected, unsuppressed findings, including
those omitted from the output; accepting a baseline does not suppress failures.

Without an explicit project schema, the inferred layout does not require the
bundled scaffold's entrypoints, core files, collection indexes or a first history
snapshot. Explicit schemas retain those requirements, including older schemas.
Only when **all** configured, non-ignored entrypoints are missing does
`missing-entrypoint-doc` become an error; an individually missing entrypoint is a
warning in an explicit layout. Broken links and malformed YAML frontmatter are
errors in either layout. `check` reports schema syntax/validation failures as
`schema-parse-error` errors without evaluating coverage or updating the baseline.

Stale scope checks use the last Git commit for each document/scope, resolved in
one batch and cached by HEAD and paths. Uncommitted files without commit times
are skipped. Without Git, mtime is only informational. Decisions and historical
documents are skipped, and multiple stale scopes share one finding per document.
Tracked documentation does not need local isolation; local layouts honor both
`.gitignore` and `.git/info/exclude`, including Git's negation rules. Reference
citations only produce informational findings for Markdown links, and orphan
documents are grouped by directory.

Suppressions using `rule` accept repository-relative path globs and a reason.
Existing named entries with `rule_id` retain exact path matching, including
literal glob characters in filenames. Both forms survive schema roundtrips:

```yaml
suppressions:
  - rule: canonical-links-reference
    path: 'docs/design-docs/**/*.md'
    reason: 'These links deliberately cite supporting evidence.'
```

#### Environment Diagnostics

```bash
harnesskit doctor [target_dir] [--json] [--layout] [--write-schema] [--force]
harnesskit doctor [target_dir] --usage [--days N] [--json]
```

Checks the CLI, git, target path, `.harnesskit/state`, `.harnesskit/history`, installed agent skills, and PATH status. SQLite is bundled. Installed skill metadata is compared with the bundled version; mismatches include update or sync instructions. Use this when first installing or when a sandboxed environment reports a read-only fact store.

`--layout` explains the inferred mappings and coverage without creating an index.
`--write-schema` saves them to `.harnesskit/schema.yaml`; when that file exists it
only previews the proposed contents until `--force` is supplied. `--preview`
always prevents the write. The schema retains existing rules and suppressions.

Both `doctor` and `check` flag possible orphan `.harnesskit` directories whose parent has no configured docs root or entrypoints, including nested accidental targets. The scan skips symlinks, hidden directories, `target`, and `node_modules`, and respects nested projects' schemas. Verify the intended project root and preserve `.harnesskit/history` before cleaning up any state.

#### Local History

```bash
harnesskit history status                          # Changed since last snapshot?
harnesskit history snapshot -m "<message>"         # Create a checkpoint
harnesskit history snapshot --allow-empty         # Force an unchanged checkpoint
harnesskit history list                            # List all snapshots
harnesskit history diff <snapshot-id|latest>       # What changed since snapshot
harnesskit history diff <snap-a> <snap-b>          # Diff between two snapshots
harnesskit history restore <snapshot-id> [--apply] # Preview or restore a snapshot
harnesskit history show <snapshot-id|latest> <path> # Print a checkpoint's file
harnesskit history export <file.tar.gz>            # Export snapshots and objects
harnesskit history import <file.tar.gz>            # Merge a portable history archive
```

Lightweight local versioning for docs — not a replacement for git, but useful for doc-level checkpoints during complex tasks.

Snapshots include every managed document, including inferred notes and `reference/`
documents, plus the local schema when present. New snapshots persist their coverage;
older snapshots remain readable by list, diff, show and restore. Unchanged file contents
skip snapshot creation with a hint; `--allow-empty` forces a checkpoint. File mtimes,
messages and Git state alone do not create a new checkpoint.

`history status` reports snapshot age in days, changed and unmanaged file counts,
and how many managed documents Git tracks. It points out documents for which history
is the only version record. Non-Git directories report `host_git_dirty: null`.
`check` warns about dirty history only when the snapshot is older than seven days
and documents changed, or more than twenty files changed; schema-only changes
retain their separate finding.

Export writes a new gzip-compressed tar archive without external programs and refuses
to overwrite an existing file. Import validates objects and snapshot paths before
merging, preserves existing snapshot IDs, and keeps the newer latest checkpoint.
It imports history only; use `history restore` to apply documents. All history
subcommands accept `--json` with coverage and next-step hints; plain `show` writes
the stored bytes without adding a newline.

#### Update

```bash
harnesskit update                         # Update to the latest release
harnesskit update --version <release-tag> # Update to a pinned release
```

</details>

## Exit Codes and Target Directories

| Code | Meaning |
|:--|:--|
| `0` | Normal completion, including help/version and a closed output pipe (for example, `harnesskit list-docs \| head`). |
| `1` | `check` found an unsuppressed error (including malformed schema/frontmatter, broken links or all explicitly configured entrypoints missing), or an unsuppressed warning with `--strict` (including schema strict mode). Baselines and output truncation do not change this. |
| `2` | Usage or environment error: invalid arguments, missing or non-directory target, unreadable schema, or failed I/O. Schema syntax errors in commands other than `check` also return 2. |

Only `init` may create a missing target directory; `init --preview` never writes files. Other commands validate `target_dir` before creating state and suggest checking whether an argument was mistaken for the target. Commands operating on a directory without configured docs or entrypoints emit a one-line hint on stderr suggesting `harnesskit init --preview`; they can still inspect or restore existing history. Help, version, and update do not operate on a project target.

For `history diff <a> <b> [target_dir]`, an explicit third argument is always the target. With two arguments, an existing directory is the target; `latest` or an existing local snapshot ID selects a second snapshot; other values are validated as targets. Use a third argument of `.` to unambiguously request a comparison with an unknown snapshot ID.

JSON reports from `index`, `check`, `list-docs`, `history`, `doctor`, `query`, `graph`, and `context` include structured `coverage` and `hints`. Query, graph and context still use only managed documents. Diagnostics on stderr do not invalidate JSON on stdout. New history snapshots add coverage metadata without requiring it in older snapshots; coverage on `history snapshot --json` describes the current invocation, including when creation is skipped.

## Local Usage Metrics

Each command appends local metrics to `~/.harnesskit/usage.jsonl`, or
`$HARNESSKIT_HOME/usage.jsonl` when configured. Logging is **on by default** and
records time, version, command, a project-root hash, elapsed time, exit code and
available query/check/coverage counts. It omits raw paths, document contents and
query text. Nothing is sent to a server. Log write failures do not fail commands.

```bash
export HARNESSKIT_USAGE_LOG=off       # Disable logging for this shell
harnesskit doctor --usage --days 30  # Summarize existing local records
```

`HARNESSKIT_USAGE_LOG=full` additionally records query text. Unset the variable to
return to minimal logging. Disabling logging leaves existing records in place.
Usage summaries show per-project counts, zero-hit and relaxed-hit rates, and
redundant index/check or repeated version calls. Exit-2 invocations count as
invalid usage and are excluded from project aggregates.

## Upgrading to 0.1.0-alpha.7

Existing schema files work unchanged; `schema_version` remains 0. Incompatible
derived `facts.sqlite` indexes rebuild automatically on the next indexed command.
Keep `.harnesskit/history`: old snapshots still support list, diff and restore.
New optional schema fields cover include/ignore globs, suppression globs and
lifecycle thresholds; no document relocation or reinitialization is required.

Review the [migration guide](docs/migration-alpha.7.md) before updating scripts:
check now uses summary output and meaningful failure codes, nonexistent targets
fail instead of being created, search may include partial matches, and usage
logging defaults to on. The [changelog](CHANGELOG.md) lists the user-visible changes.

## How the Skill Works

The agent skill is a short, versioned shared file with an on-demand CLI reference:

1. **When to trigger** — Any task involving project docs, architecture, design, plans, or specs
2. **How to orient** — Read `AGENTS.md` or `CLAUDE.md` first, then follow progressive disclosure
3. **When to use the CLI** — Start handoffs with `context --current`; use 1–3 keywords in `query` and follow zero-hit hints; run one `check` after edits and snapshot at phase boundaries
4. **How to update docs** — Edit the narrowest relevant doc, create from templates when needed, keep entrypoints thin

The source is `agent-surfaces/skills/harnesskit/SKILL.md`; install the entire
directory, including `references/`. `doctor` detects installed version drift;
use `harnesskit update` for release installs or the checkout's skill install
scripts to sync. Current state belongs only in `docs/project.md`; entrypoints
remain routing tables. A successful docs check does not replace project tests,
runtime checks or owner acceptance, and CLI statistics do not belong in worklogs.

## Project Structure

```
src/               Rust CLI implementation
schemas/           Doc scaffold schema (file-first-v0.yaml)
templates/         Entrypoint, core doc, and collection templates
agent-surfaces/    Shared agent skill + Codex / Claude Code packaging
scripts/           Install, uninstall, sync, and release scripts
examples/          Example initialized repos
docs/              Commands and migration notes
tests/fixtures/    Synthetic repositories and frozen compatibility evidence
```

## Examples

- `examples/web-app-with-auth/` shows a small auth documentation graph with a product spec, design doc, decision record, and sample context packet shape.

Try it after building or installing HarnessKit:

```bash
harnesskit context --current examples/web-app-with-auth --json
harnesskit query "auth session" examples/web-app-with-auth --limit 5 --json
harnesskit context auth examples/web-app-with-auth --json
```

## What HarnessKit Is Not

- **Not an agent runtime.** It doesn't run models, manage threads, or call tools.
- **Not a RAG pipeline.** No embeddings, no vector stores. Just FTS5 and file structure.
- **Not a SaaS product.** Everything runs locally. No accounts or uploaded telemetry; local usage logging can be disabled.
- **Not a replacement for git.** The history feature is for doc-level checkpoints, not source control.

---

<div align="center">

**[Latest Release](https://github.com/qWaitCrypto/HarnessKit/releases/latest)** · **[Installer](install.sh)** · **[Schema Reference](schemas/file-first-v0.yaml)** · **[Agent Skill](agent-surfaces/skills/harnesskit/SKILL.md)**

MIT License · Built by [qWait](https://github.com/qWaitCrypto)

</div>
