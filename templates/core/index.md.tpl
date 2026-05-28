# Docs Index

Authoritative manifest for the managed docs tree. Update it when the set of main entry docs changes.

## Reading Order

1. `AGENTS.md` or `CLAUDE.md`
2. `{{architecture_path}}`
3. `{{manifest_path}}`
4. Relevant collection index
5. The smallest relevant focused document

Do not read the whole docs tree by default.

## Core Entry Points

| Path | Role | Status |
| --- | --- | --- |
{{core_rows}}

## Collections

| Path | Purpose | Default Authority | Default Status | Create Missing Docs From |
| --- | --- | --- | --- | --- |
{{collection_rows}}

## Template Library

| Path | Use |
| --- | --- |
{{template_rows}}

## Update Rules

- Keep this file as a real manifest, not a blank directory listing.
- Add focused docs to their collection index before promoting them here.
- Update a collection index when a doc becomes a repeated reading entrypoint.
