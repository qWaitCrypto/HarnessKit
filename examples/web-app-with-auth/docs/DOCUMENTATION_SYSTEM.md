# Documentation System

How this repository uses HarnessKit docs.

## Managed Root

- Managed docs root: `docs/`
- Top-level manifest: `docs/index.md`
- Project map: `docs/project.md`

## Layers

- Entry: `AGENTS.md` / `CLAUDE.md`
- Repo map: `ARCHITECTURE.md`
- Docs manifest: `docs/index.md`
- Collection manifests: one `index.md` per managed collection
- Focused docs: product specs, design docs, decisions, plans, worklogs, operations, generated artifacts, references

## Principles

- File as memory
- Keep entrypoints short and route outward
- Keep anchor docs focused and lightweight docs light
- Prefer directory defaults over repeated frontmatter
- Prefer explicit links and paths over hand-maintained relation blobs

## Update Rules

- Update docs when semantics change
- Update `AGENTS.md` / `CLAUDE.md` only when routing rules, reading order, or global repo constraints change
- Update `ARCHITECTURE.md` whenever the stable high-level map changes, but keep its role, structure, and length tight
- Keep process docs lightweight
- Use `docs/templates/` when a focused doc is missing
- Update the relevant collection index when a doc becomes a repeated reading entrypoint
- Prefer replacing stale docs over leaving contradictions
