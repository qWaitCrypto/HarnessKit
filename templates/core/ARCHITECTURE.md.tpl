# ARCHITECTURE

High-level map of the repository. Keep this file short, stable, and focused on boundaries.

Update this file when the high-level structure changes, but preserve the same role, section structure, and concise format. Do not let it grow into a long design document.

## Purpose

State what this repository exists to do in a few stable paragraphs.

## Where Things Live

- Product intent and user-visible behavior: `{{product_specs_index_path}}`
- Technical design and tradeoffs: `{{design_docs_index_path}}`
- Durable decisions: `{{decisions_index_path}}`
- Current project map: `{{project_map_path}}`
- Validation paths: `{{commands_path}}`

## Main Areas

List the major code or subsystem areas and the paths they own.

## Stable Boundaries

Record the layer boundaries, dependency directions, or module invariants that should survive refactors.

## Deliberately Absent

Record what this repository intentionally does not do here, and which dependencies or couplings should stay absent.

## Reading Path

- Start here for project orientation.
- Then read `{{manifest_path}}`.
- Then read the relevant collection index.
- Then read the smallest relevant focused document.
