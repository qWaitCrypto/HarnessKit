# HarnessKit migration review

`harnesskit migrate --plan --json` reports deterministic upgrades and items
that need an agent or owner to decide. Read the `evidence` and `action` for
each item; category names are `schema`, `layout`, `lifecycle`, `anchors`,
`gitignore`, and `knowledge-lock`.

Zero-config projects do not need a schema file. A missing schema is not a
migration item; optional layout mappings can be reviewed without adopting
them. Doctor gives one migration hint only for upgrades to an explicit
project schema. Context has no reliable session id, so it does not repeat
this hint or run migration analysis.

Safe application is intentionally narrow:

```text
harnesskit migrate --apply [TARGET_DIR]
harnesskit migrate --apply --only schema [TARGET_DIR]
```

The command snapshots managed files before writing. It may update
`.harnesskit/schema.yaml`, append `.harnesskit/state/` to `.gitignore`, and
initialize `.harnesskit/knowledge.lock` when a document has a resolved
explicit code anchor and the lock is missing. Existing schema files receive
targeted text edits that preserve comments, quotes, key order and line
endings. The Markdown body is never rewritten. A second apply is idempotent
and reports that no safe changes are needed.

Lifecycle and anchor entries are review work. For a suspected replacement,
confirm the relationship, then edit only frontmatter (`status`,
`superseded_by`, or `supersedes_parts_of`) and preserve the prose. For an
anchor, verify the suggested file/symbol and evidence, then use
`harnesskit suggest-anchors <doc> --apply <N>` so the prose change is explicit.
The plan groups anchors by document and previews up to three candidates with
evidence summaries; use `suggest-anchors <doc>` for its current candidate
numbers. Both commands exclude ambiguous mentions and use the existing
Weave confidence floor (cochange weight at least 0.2).
Do not accept a lifecycle clue solely because a path contains `completed` or
an opening paragraph says “superseded”.

After each accepted decision, run:

```text
harnesskit check --json
```

Review errors first, then warnings and the remaining migration plan. Keep the
history snapshot created by `migrate --apply` until the migration is reviewed.
