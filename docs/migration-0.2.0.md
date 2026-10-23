# Migration to 0.2.0

Upgrade the CLI and the shared agent surface together, including
`references/`. For a release install, use `harnesskit update --version v0.2.0`;
from a source checkout, use `scripts/install-local-cli.sh` and
`scripts/sync-agent-surfaces.sh` (or the individual skill installers). The
bundled skill metadata and plugin manifest must report the same `0.2.0` version.

## Compatibility

- Existing `.harnesskit/schema.yaml` files with `schema_version: 0` remain
  usable. The schema version is independent of the CLI version; upgrading is
  optional until the new schema fields are needed.
- Zero-config repositories remain zero-config. They do not need a schema,
  `.harnesskit/knowledge.lock`, or an initialization scaffold, and `migrate`
  does not turn layout suggestions into required changes.
- Derived `.harnesskit/state/` data may be rebuilt automatically. Existing
  `.harnesskit/history` snapshots remain readable by `list`, `diff` and
  `restore`.

## Structured migration

Review the deterministic plan before applying it:

```text
harnesskit migrate --plan [TARGET_DIR] [--json]
harnesskit migrate --apply [TARGET_DIR] [--only schema|layout|lifecycle|anchors|gitignore|knowledge-lock]
```

`--apply` takes a history snapshot first and only edits schema/frontmatter-shaped
metadata, `.gitignore`, and the durable knowledge lock. It never rewrites
Markdown prose and a second apply is idempotent. Lifecycle clues and anchor
suggestions remain review work; inspect their evidence, then use
`harnesskit check --ack <doc>`, `harnesskit check --fix`, or
`harnesskit suggest-anchors <doc> --apply <N>` as appropriate.

## New optional fields

When an explicit schema is upgraded, the plan may add `schema_version: 1`,
`rules.entrypoint_max_lines` (default `120`) and
`rules.stale_active_plan_days` (default `30`). Document collections may allow
`superseded_by` and `supersedes_parts_of`; active plan and worklog collections
also allow `status`. Inferred layouts can be recorded with `include` mappings,
and suppressions can use the glob-aware `rule` key instead of legacy `rule_id`.

## Behavior changes

- Retrieval trims weak results relative to the best hit: document candidates
  use a `0.30` floor and mixed knowledge bundles use a `0.50` floor when a
  strong document is present. This keeps low-signal code from filling the
  output budget.
- `suggest-anchors` and migration anchor suggestions accept inferred co-change
  evidence only at weight `0.2` or higher. Ambiguous mentions stay review-only.
- Agent hooks are disabled by default. `harnesskit hook pre-edit|post-edit`
  is inert until an agent invokes it, and `harnesskit install --hooks --write`
  explicitly enables project-local Claude Code settings.

## knowledge.lock and Git

Commit `.harnesskit/knowledge.lock` when the repository uses explicit code
anchors. It is the small, reviewable source file that seals the code state
confirmed by documentation. Keep `.harnesskit/state/` ignored because it is a
derived SQLite/cache layer. `migrate --apply` initializes the lock only when
resolved anchors exist; edits to it are visible in the normal Git diff.

After migration, run `harnesskit index` and `harnesskit check --json`; then run
the project's tests and release gates. See [the CLI reference](../agent-surfaces/skills/harnesskit/references/cli.md)
and [the alpha.7 migration notes](migration-alpha.7.md) for older compatibility
details.
