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
| **SQLite is derived** | The `.harnesskit/` index is built from docs. Delete it anytime — rebuild with one command. |
| **Built for agents** | The CLI is a tool surface that agents call. Users talk to their agent in natural language; the agent picks the right command. |
| **Not a runtime** | HarnessKit does not run models, manage conversations, or execute tools. It's the engineering harness, not the agent harness. |
| **Lightweight** | Single Rust binary. No daemons. No MCP servers. No background processes. Install and forget. |

## Quick Start

**Step 1** — Install the CLI and agent skill:

```bash
# Public alpha install
curl -fsSL https://raw.githubusercontent.com/qWaitCrypto/HarnessKit/main/install.sh | bash
```

For reproducible installs, pin a release tag:

```bash
curl -fsSL https://raw.githubusercontent.com/qWaitCrypto/HarnessKit/main/install.sh | bash -s -- --version v0.1.0-alpha.2
```

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

- The public binary release currently supports Linux x86_64.
- The installer writes the Codex skill directly; Codex plugin packaging is optional.
- The installer can be rerun with a newer `--version` tag to upgrade.
- After alpha.2, `harnesskit update` can update the CLI and already-installed skills from GitHub releases.

**Step 2** — Ask your agent to set up project docs:

```
> Initialize project documentation for this repo.
```

The agent runs `harnesskit init`, reads the entrypoint, and starts managing your project docs. That's it.

**Step 3** — Work naturally:

```
> What was the design rationale for the auth module?
> Create a design doc for the new payment flow.
> Are our docs consistent? Anything broken?
> Snapshot the current docs before I refactor.
```

The agent calls the CLI behind the scenes. You never need to.

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
└── .harnesskit/                 # Internal state (SQLite index, history)
```

Agents navigate this through **progressive disclosure**: entrypoint → architecture → manifest → collection index → the smallest doc that answers the task. They never bulk-read everything.

<details>
<summary><b>CLI Reference</b> (for agents — or curious humans)</summary>

<br>

#### Initialization

```bash
harnesskit init [target_dir] [--schema <path>] [--docs-root <path>] [--force]
```

Creates the docs scaffold. Existing files are skipped unless `--force` is passed. Automatically adds managed paths to `.git/info/exclude` for local isolation.

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

#### Health Check

```bash
harnesskit check [--json] [--strict] [--rules <rule-list>]
```

Validates all managed docs — missing required files, broken links, stale anchors, duplicate content, frontmatter issues, and more.

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
