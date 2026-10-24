---
name: harnesskit
description: Use for project documentation, architecture, plans, decisions, and durable repo knowledge; follow existing AGENTS.md, CLAUDE.md, or .harnesskit conventions.
metadata:
  version: "0.2.0"
---

# HarnessKit

Files are project memory; the CLI retrieves, checks, migrates and checkpoints it.

## Before working

- Read `AGENTS.md` or `CLAUDE.md`; follow its routes to the smallest relevant document. Do not read the whole docs tree.
- Takeover: `harnesskit context --current` shows active work and recent changes.
- Evidence: `harnesskit query "1–3 distinctive keywords"`; on zero results follow `hints`, then use `rg`.
- Before code edits: `harnesskit context --for <path|symbol>`; read the full constraining section, not just the snippet.
- Conflicting feature requests do not override accepted decisions. Honor their caller/boundary and name the decision in your reply, or stop with the decision path and options. Never rewrite accepted bodies to fit; replacement needs explicit user consent and a new superseding decision.
- Review: `harnesskit context --diff [rev]` shows affected documents and drift.

## After working

- Update the existing knowledge owner; avoid parallel docs. Mark approved replacements with `status: superseded` and `superseded_by`.
- Reference related code paths/symbols in backticks.
- Run `harnesskit check` once; `check --ack <doc>` confirms unchanged requirements, `check --fix` repairs moved anchors. Preserve accepted decision bodies.
- A clean check does not verify delivery; run project tests and acceptance checks.
- Keep current state in `docs/project.md`; entrypoints route outward.
- No tool installation or HarnessKit usage in product README/docs; one AGENTS routing line suffices.
- Existing docs: follow inventory → plan → move → check in [references/adoption.md](references/adoption.md).

Command options, hooks and version sync: [references/cli.md](references/cli.md). Upgrading an older project: [references/migration.md](references/migration.md).
