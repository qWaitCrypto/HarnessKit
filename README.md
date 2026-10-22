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

The agent calls the CLI behind the scenes. You never need to.

## 30-Second Demo

When an agent needs project context, it should not scan every doc. It asks HarnessKit for a context packet:

```bash
harnesskit context auth --json
```

Shape of the answer:

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
harnesskit index
```

Refreshes the SQLite fact store from current docs. Incremental — only processes changed files.

#### Search & Context

```bash
harnesskit query "<terms>" [--json]       # Full-text search across all docs
harnesskit context "<terms-or-path>"      # Context packet with relation graph
harnesskit graph <doc_path> [--json]      # Context packet for a specific doc
```

#### Inspection

```bash
harnesskit inspect <doc_path>    # Full metadata, relations, and checks for a doc
harnesskit refs <doc_path>       # All incoming references to a doc
```

#### Discovery

```bash
harnesskit list-docs    # All managed docs with metadata
harnesskit rank         # Top 20 most-referenced docs
```

Discovery scans all Markdown under `managed_root`, plus configured core files and
entrypoints. It skips templates, hidden directories (including `.harnesskit`),
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
matches. `hints` explain the scope and provide schema include snippets for unmanaged
directories.

`check` reports missing local link targets as `broken-link` errors, and existing
unmanaged Markdown as one `unmanaged-docs` warning per directory. Existing files
outside the docs root referenced by relative Markdown links or inline paths are
hashed in the derived `external_refs` table. An `external-ref-changed` info finding
names the referring document when those bytes change. It persists across commands
until that document is updated; rebuilding state resets the hash baseline. External
references are never added to history unless explicitly included as managed Markdown.

#### Health Check

```bash
harnesskit check [--json] [--strict] [--rules <rule-list>]
```

Validates all managed docs — missing required files, broken links, stale anchors, duplicate content, frontmatter issues, and more.

#### Environment Diagnostics

```bash
harnesskit doctor [--json] [--layout] [--write-schema] [--force]
```

Checks the CLI, git, target path, `.harnesskit/state`, `.harnesskit/history`, installed agent skills, and PATH status. SQLite is bundled. Use this when first installing or when a sandboxed environment reports a read-only fact store.

`--layout` explains the inferred mappings and coverage without creating an index.
`--write-schema` saves them to `.harnesskit/schema.yaml`; when that file exists it
only previews the proposed contents until `--force` is supplied. `--preview`
always prevents the write. The schema retains existing rules and suppressions.

Both `doctor` and `check` flag possible orphan `.harnesskit` directories whose parent has no configured docs root or entrypoints, including nested accidental targets. The scan skips symlinks, hidden directories, `target`, and `node_modules`, and respects nested projects' schemas. Verify the intended project root and preserve `.harnesskit/history` before cleaning up any state.

#### Local History

```bash
harnesskit history status                          # Changed since last snapshot?
harnesskit history snapshot -m "<message>"         # Create a checkpoint
harnesskit history list                            # List all snapshots
harnesskit history diff <snapshot-id|latest>       # What changed since snapshot
harnesskit history diff <snap-a> <snap-b>          # Diff between two snapshots
harnesskit history restore <snapshot-id> [--apply] # Preview or restore a snapshot
```

Lightweight local versioning for docs — not a replacement for git, but useful for doc-level checkpoints during complex tasks.

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
| `1` | `check` found an unsuppressed error, or an unsuppressed warning with `--strict` (including schema strict mode). |
| `2` | Usage or environment error: invalid arguments, missing or non-directory target, unreadable schema, or failed I/O. |

Only `init` may create a missing target directory; `init --preview` never writes files. Other commands validate `target_dir` before creating state and suggest checking whether an argument was mistaken for the target. Commands operating on a directory without configured docs or entrypoints emit a one-line hint on stderr suggesting `harnesskit init --preview`; they can still inspect or restore existing history. Help, version, and update do not operate on a project target.

For `history diff <a> <b> [target_dir]`, an explicit third argument is always the target. With two arguments, an existing directory is the target; `latest` or an existing local snapshot ID selects a second snapshot; other values are validated as targets. Use a third argument of `.` to unambiguously request a comparison with an unknown snapshot ID.

JSON reports from `index`, `check`, `list-docs`, `history snapshot`, `doctor`, `query`, `graph`, and `context` include structured `coverage` and `hints`. Query, graph and context still use only managed documents. Diagnostics on stderr do not invalidate JSON on stdout. The on-disk history snapshot format is unchanged; coverage on `history snapshot --json` describes that invocation.

## How the Skill Works

The agent skill is a single shared file that teaches Codex and Claude Code the HarnessKit workflow:

1. **When to trigger** — Any task involving project docs, architecture, design, plans, or specs
2. **How to orient** — Read `AGENTS.md` or `CLAUDE.md` first, then follow progressive disclosure
3. **When to use the CLI** — Prefer `query` / `context` over manual file search; run `check` after doc changes; use `history` before risky edits
4. **How to update docs** — Edit the narrowest relevant doc, create from templates when needed, keep entrypoints thin

The skill lives at `agent-surfaces/skills/harnesskit/SKILL.md` — one source of truth for both Codex and Claude Code.

## Project Structure

```
src/               Rust CLI implementation
schemas/           Doc scaffold schema (file-first-v0.yaml)
templates/         Entrypoint, core doc, and collection templates
agent-surfaces/    Shared agent skill + Codex / Claude Code packaging
scripts/           Install, uninstall, sync, and release scripts
examples/          Example initialized repos
docs/              Internal development docs
```

## Examples

- `examples/web-app-with-auth/` shows a small auth documentation graph with a product spec, design doc, decision record, and sample context packet shape.

Try it after building or installing HarnessKit:

```bash
harnesskit index examples/web-app-with-auth
harnesskit context auth examples/web-app-with-auth --json
```

## What HarnessKit Is Not

- **Not an agent runtime.** It doesn't run models, manage threads, or call tools.
- **Not a RAG pipeline.** No embeddings, no vector stores. Just FTS5 and file structure.
- **Not a SaaS product.** Everything runs locally. No accounts, no cloud, no telemetry.
- **Not a replacement for git.** The history feature is for doc-level checkpoints, not source control.

---

<div align="center">

**[Latest Release](https://github.com/qWaitCrypto/HarnessKit/releases/latest)** · **[Installer](install.sh)** · **[Schema Reference](schemas/file-first-v0.yaml)** · **[Agent Skill](agent-surfaces/skills/harnesskit/SKILL.md)**

MIT License · Built by [qWait](https://github.com/qWaitCrypto)

</div>
