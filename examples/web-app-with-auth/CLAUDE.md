# CLAUDE.md

This repository uses HarnessKit.
Use the HarnessKit skill for project docs, design, plans, and durable project memory.

## Start Here

1. Read `CLAUDE.md`.
2. Read `ARCHITECTURE.md`.
3. Read `docs/index.md`.
4. Read the relevant collection index.
5. Read only the smallest relevant document for the task.

Do not read the whole docs tree by default.

## Progressive Disclosure

1. Orientation: `CLAUDE.md` -> `ARCHITECTURE.md`
2. Manifest: `docs/index.md`
3. Collection manifest: `docs/product-specs/index.md`, `docs/design-docs/index.md`, `docs/decisions/index.md`, `docs/exec-plans/active/index.md`, or `docs/worklog/active/index.md`
4. Focused doc: the smallest relevant doc inside that collection
5. Reference: `docs/generated/index.md` or `docs/references/index.md` only when needed

Escalate to the next layer only when the current layer is insufficient.

## Task-Based Reading Paths

- Feature work: `ARCHITECTURE.md` -> `docs/product-specs/index.md` -> relevant product spec -> `docs/design-docs/index.md` -> relevant design doc
- Refactor: `ARCHITECTURE.md` -> `docs/design-docs/index.md` -> relevant design doc -> `docs/exec-plans/active/index.md` if active work already exists
- Bug fix: `docs/product-specs/index.md` or `docs/design-docs/index.md` -> relevant focused doc -> `docs/worklog/active/index.md` or `docs/exec-plans/active/index.md` if needed
- Docs work: `docs/index.md` -> relevant collection index -> narrow target doc

If no relevant document exists, create the narrowest missing doc from `docs/templates/`, then update the relevant collection index.

## Source Of Truth

- Architecture map: `ARCHITECTURE.md`
- Docs map: `docs/index.md`
- Project map: `docs/project.md`
- Documentation rules: `docs/DOCUMENTATION_SYSTEM.md`
- Commands: `docs/commands.md`

## Update Rules

- Keep this file short.
- Keep durable knowledge in focused docs.
- Prefer updating docs when semantics change, not on every small diff.
