---
name: harnesskit
description: Use for project documentation, architecture, plans, decisions, and durable repo knowledge; follow existing AGENTS.md, CLAUDE.md, or .harnesskit conventions.
metadata:
  version: "0.1.0-alpha.7"
---

# HarnessKit

## When to use

Use for reading or changing durable project knowledge. Repository files are the source of truth; the CLI handles deterministic retrieval, checks and checkpoints.

## Read the entrypoint first

Read `AGENTS.md` (Codex) or `CLAUDE.md` (Claude), then follow its routes through `ARCHITECTURE.md`, the docs index and the smallest relevant document. Do not read the whole docs tree.

## Common commands

- Taking over work: `harnesskit context --current` for current pointers, active plans and snapshot changes.
- Finding evidence: `harnesskit query "1–3 distinctive keywords" --json`; on zero results, follow `hints` to change terms, then use `rg` if needed.
- After editing docs: run `harnesskit check --json` once; it refreshes the index automatically.
- At a phase boundary: `harnesskit history snapshot -m "what changed"` to preserve a checkpoint.

Read CLI `coverage` and `hints` to understand scope and next steps. A check with no errors does not verify delivery: run the project's required tests and acceptance checks.

## When to write docs

Capture changed semantics, decisions, operating knowledge or handoff state in the narrowest existing doc; update its index when needed. Keep current state only in `docs/project.md` (or the configured project map), and keep entrypoints as routing tables. Do not copy CLI statistics into worklogs.

For setup, command options, recovery and installation/version sync, read [references/cli.md](references/cli.md) only when needed.
