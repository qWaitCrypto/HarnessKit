---
name: harnesskit
description: Use for project documentation, architecture, plans, decisions, and durable repo knowledge; follow existing AGENTS.md, CLAUDE.md, or .harnesskit conventions.
metadata:
  version: "0.2.0"
---

# HarnessKit

Repository files are the source of truth; the CLI gives deterministic retrieval, checks, migrations and checkpoints.

## Before working

- Read `AGENTS.md` (Codex) or `CLAUDE.md` (Claude), then follow its routes to the smallest relevant document. Do not read the whole docs tree.
- Taking over work: `harnesskit context --current` for current pointers, active plans and recent changes.
- Finding evidence: `harnesskit query "1–3 distinctive keywords"`; on zero results follow `hints`, then use `rg`.
- Before editing code: `harnesskit context --for <path|symbol>` shows the decisions, pitfalls and plans that constrain it. Treat decisions as binding unless the user overrides them.
- Reviewing a change: `harnesskit context --diff [rev]` lists affected documents and anchors that will drift.

## After working

- Update the narrowest existing document that owns the changed knowledge; do not create a parallel doc. Mark replaced docs with `status: superseded` and `superseded_by`.
- Reference related code paths or symbols in backticks so they become anchors.
- Run `harnesskit check` once. For drift, update the doc or run `check --ack <doc>` if the code change does not affect it; `check --fix` repairs uniquely moved anchors.
- A clean check does not verify delivery: run the project's tests and acceptance checks.
- Keep current state in the project map (`docs/project.md`) and entrypoints as routing tables.

Command options, setup, hooks and version sync: [references/cli.md](references/cli.md). Upgrading an older project: [references/migration.md](references/migration.md).
