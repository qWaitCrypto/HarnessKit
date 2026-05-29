# CLAUDE.md

This repository uses HarnessKit.
Use the HarnessKit skill for project docs, design, plans, and durable project memory.
For documentation-related work, use the HarnessKit skill first.

## Start Here

1. Read `CLAUDE.md`.
2. Read `{{architecture_path}}`.
3. Read `{{manifest_path}}`.
4. Read the relevant collection index.
5. Read only the smallest relevant document for the task.

Do not read the whole docs tree by default.

## Progressive Disclosure

1. Orientation: `CLAUDE.md` -> `{{architecture_path}}`
2. Manifest: `{{manifest_path}}`
3. Collection manifest: `{{product_specs_index_path}}`, `{{design_docs_index_path}}`, `{{decisions_index_path}}`, `{{exec_plans_active_index_path}}`, or `{{worklog_active_index_path}}`
4. Focused doc: the smallest relevant doc inside that collection
5. Reference: `{{generated_index_path}}` or `{{references_index_path}}` only when needed

Escalate to the next layer only when the current layer is insufficient.

## Task-Based Reading Paths

- Feature work: `{{architecture_path}}` -> `{{product_specs_index_path}}` -> relevant product spec -> `{{design_docs_index_path}}` -> relevant design doc
- Refactor: `{{architecture_path}}` -> `{{design_docs_index_path}}` -> relevant design doc -> `{{exec_plans_active_index_path}}` if active work already exists
- Bug fix: `{{product_specs_index_path}}` or `{{design_docs_index_path}}` -> relevant focused doc -> `{{worklog_active_index_path}}` or `{{exec_plans_active_index_path}}` if needed
- Docs work: `{{manifest_path}}` -> relevant collection index -> narrow target doc

If no relevant document exists, create the narrowest missing doc from `{{docs_root}}/templates/`, then update the relevant collection index.

## Source Of Truth

- Architecture map: `{{architecture_path}}`
- Docs map: `{{manifest_path}}`
- Project map: `{{project_map_path}}`
- Documentation rules: `{{docs_root}}/DOCUMENTATION_SYSTEM.md`
- Commands: `{{commands_path}}`

## Update Rules

- Keep this file short.
- Keep durable knowledge in focused docs.
- Prefer updating docs when semantics change, not on every small diff.
