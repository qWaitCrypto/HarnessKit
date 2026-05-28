---
name: harnesskit
description: >
  Use whenever the task involves project documentation, architecture, design decisions,
  implementation plans, specs, worklogs, project memory, or any durable knowledge
  that should live in the repo. This includes reading or writing docs, querying prior
  decisions, checking doc health, reviewing history, updating AGENTS.md / CLAUDE.md /
  ARCHITECTURE.md, or organizing project knowledge. If the repo has AGENTS.md,
  CLAUDE.md, or .harnesskit/, treat HarnessKit as the default documentation workflow:
  read the entrypoint first, then use the CLI when query, context, checks, indexing,
  or history are useful.
---

# HarnessKit Skill

## What HarnessKit Is

HarnessKit is a lightweight repo-level project harness, not an agent runtime. It follows the file-as-memory model: repository files are the source of truth for project knowledge, and agents read, query, and update them through normal file operations and the `harnesskit` CLI.

Key concepts:
- **Repository files are memory.** AGENTS.md, CLAUDE.md, ARCHITECTURE.md, docs/ — these are the persistent knowledge layer, not chat history or external databases.
- **SQLite is a derived index.** The `.harnesskit/` directory contains an incremental fact store (SQLite + content-addressed history). It can be deleted and rebuilt from docs at any time. Never treat it as primary data.
- **CLI is the deterministic action boundary.** The `harnesskit` CLI handles indexing, querying, checking, and history — operations that require consistency guarantees. Normal doc edits are done by the agent directly.

This architecture is permanent: skill defines workflow, CLI executes deterministic actions, agent reads and edits files. Do not build MCP servers, daemons, tool wrappers, or large command menus around HarnessKit.

## Prerequisite

The `harnesskit` CLI must be on `PATH` or runnable via `cargo run --` in the project root.

Before running a deterministic HarnessKit CLI action, verify the CLI is available:

```bash
harnesskit --version
```

If this fails, tell the user the CLI must be installed before CLI-backed actions can run. Normal file reading and focused doc edits can still proceed when the user asked for them.

## Phase 1: Initialization

### When to Init

Run `harnesskit init` when:
- The user asks to set up HarnessKit or project docs
- The user asks to initialize a docs system or project memory

Do not initialize a repository merely because this skill was triggered. Treat initialization as a project setup action that needs an explicit user request or clear user authorization.

### Running Init

```bash
harnesskit init [target_dir] [--schema <path>] [--docs-root <path>] [--force]
```

Most common case — init in the current repo root with defaults:

```bash
harnesskit init
```

Init creates a managed docs scaffold based on the schema:
- **Entrypoints**: `AGENTS.md`, `CLAUDE.md`, `ARCHITECTURE.md`
- **Docs manifest**: `docs/index.md`, `docs/project.md`, `docs/DOCUMENTATION_SYSTEM.md`, `docs/commands.md`
- **Doc collections**: `docs/product-specs/`, `docs/design-docs/`, `docs/decisions/`, `docs/exec-plans/`, `docs/worklog/`, `docs/operations/`, `docs/generated/`, `docs/references/` — each with its own `index.md`
- **Templates**: `docs/templates/` for creating new docs per collection type
- **Internal state**: `.harnesskit/state/`, `.harnesskit/history/`, `.harnesskit/schema.yaml`

Files that already exist are skipped unless `--force` is passed.

### Git Exclude

If the repo is a Git repository, init automatically adds HarnessKit-managed paths (`docs/`, `AGENTS.md`, `CLAUDE.md`, `ARCHITECTURE.md`, `.harnesskit/`) to `.git/info/exclude` for local isolation.

When init reports this, explain to the user:
> HarnessKit added its managed paths to your local git exclude (`.git/info/exclude`). This keeps them out of git status locally without modifying `.gitignore`. If you want these paths tracked by git, you can remove the entries from `.git/info/exclude` and commit the files normally.

Do not write `.gitignore` entries unless the user explicitly asks.

### Post-Init: Read the Entrypoint

After init completes, immediately read the agent entrypoint in the same session:

- **Codex**: read `AGENTS.md`
- **Claude Code**: read `CLAUDE.md`

Then follow the progressive disclosure path — do not read everything at once:

1. `AGENTS.md` or `CLAUDE.md` — orientation and reading paths
2. `ARCHITECTURE.md` — high-level code map
3. `docs/index.md` — docs manifest
4. Relevant collection index (e.g., `docs/product-specs/index.md`)
5. The smallest focused doc that answers the current task

**Stop at the layer that answers the task.** Do not read the entire docs tree.

## Phase 2: Ongoing Work

Once HarnessKit is initialized, use the CLI proactively during normal development tasks.

### Orientation in an Existing Repo

When entering a repo that already has `.harnesskit/` or `AGENTS.md` / `CLAUDE.md`:

1. Read the agent entrypoint first
2. Follow the progressive disclosure path described above
3. Use `harnesskit query` or `harnesskit context` to find relevant docs before browsing manually

### CLI Reference

All commands accept optional `[target_dir]`, `[--schema <path>]`, `[--docs-root <path>]` arguments. Defaults: target_dir = `.`, schema = bundled `file-first-v0.yaml`.

#### Indexing

```bash
harnesskit index
```

Refreshes the fact store (SQLite index + relation graph) from current docs. Run this after making doc changes when you need up-to-date query, check, or inspect results. Indexing is incremental — only changed files are re-processed.

#### Querying

```bash
harnesskit query "<terms>" --json
```

Searches the fact store using FTS5 full-text search and lexical matching. Returns ranked doc candidates with path, title, role, and relevance score. Use `--json` for structured output.

**When to use**: before manually searching docs for project context, design rationale, or prior decisions. Let the index do the work.

#### Context Retrieval

```bash
harnesskit context "<terms-or-doc-path>" --json
```

Returns a context packet for a doc: the doc itself plus its relation graph neighbors (linked docs, scoped code paths, incoming references) in recommended reading order. If given search terms instead of a path, finds the best matching doc first.

**When to use**: when you need to understand a doc in context — what it links to, what references it, what code it scopes.

#### Inspection

```bash
harnesskit inspect <doc_path>
```

Shows full metadata, relations (both incoming and outgoing), and checks for a single doc. Useful for understanding a doc's role in the knowledge graph.

```bash
harnesskit refs <doc_path>
```

Shows all incoming references (other docs that link to, index, or mention this doc).

```bash
harnesskit graph <doc_path> [--json]
```

Like `context`, but anchored on a specific doc path (no search fallback).

#### Discovery

```bash
harnesskit list-docs
```

Lists all managed docs with path, title, role, status, authority, link counts, and collection. Use for broad orientation.

```bash
harnesskit rank
```

Shows the top 20 docs by incoming-link score. Useful for finding anchor documents — the high-value docs that other docs reference most.

#### Health Check

```bash
harnesskit check [--json] [--strict] [--rules <rule-list>]
```

Runs validation rules against all managed docs and reports findings by severity (error / warning / info). Rules cover missing required docs, manifest/index drift, unresolved links, stale anchors, duplicate/overlapping content, frontmatter constraints, history state, and local isolation checks.

**When to use**: after doc changes to verify consistency, or when the user asks about doc health. Always add `--json` for structured output. Use `--strict` to treat warnings as errors (exit code 1).

Summarize findings grouped by rule id and path. Focus on errors first, then warnings.

#### History

```bash
harnesskit history status
```

Shows whether any managed docs have changed since the last snapshot. Use as a quick check before and after work.

```bash
harnesskit history snapshot -m "<message>"
```

Creates a content-addressed snapshot of all managed docs. Use before risky changes or at natural checkpoints.

```bash
harnesskit history list
```

Lists all snapshots with id, timestamp, file count, and message.

```bash
harnesskit history diff <snapshot-id|latest>
harnesskit history diff <snapshot-a> <snapshot-b>
```

Shows what changed between a snapshot and current state, or between two snapshots.

```bash
harnesskit history restore <snapshot-id|latest> [--apply] [--force]
```

Previews or applies a restore from a snapshot. Without `--apply`, shows what would change. With `--apply`, restores files after preflight checks. With `--force`, overwrites even if current files have unsaved changes.

**When to use**: history is a lightweight local versioning system (not git). Use it for doc-level checkpoints and recovery, especially for exec-plans and worklog during complex tasks.

### Updating Docs

When durable semantic changes need capture:

- Update the narrowest relevant doc and its collection index.
- Create new docs from `docs/templates/` when no existing doc covers the topic — but only when the addition is the minimal useful memory for the task.
- Keep `AGENTS.md` and `CLAUDE.md` short — they are routing tables, not encyclopedias. Detailed knowledge belongs in focused docs under `docs/`.
- After updates, run `harnesskit check --json` to verify consistency.

## Rules

- The user speaks naturally; the agent decides which CLI command fits. Do not ask users to memorize commands.
- Prefer CLI retrieval (`query`, `context`, `inspect`) over manual file search for project knowledge.
- When creating docs, use the correct collection directory and template. The schema defines which frontmatter fields each collection allows.
- Progressive disclosure is not optional — always start from the entrypoint and drill down. Never scan the full docs tree to "catch up".
