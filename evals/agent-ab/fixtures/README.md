# Historical skill fixture

`alpha7-skill/` is an unchanged copy of `agent-surfaces/skills/harnesskit/` at commit `e0deab9` (S11 alpha.7 release polish). It contains the original SKILL.md and references/cli.md. S7 installs these exact files into its isolated CODEX_HOME before the task.

`pilot1-regressions.json` captures exact before/after text from the discarded pilot1 results: S1 A/B caller retries and decision edits, plus S7 B schema/skill synchronization and frontmatter-only product changes. It includes S7's installed skill, but no credentials, sessions, binaries, or runtime history. Tests replay these deltas over the deterministic service corpus and run the real graders and hidden tests; they do not need the original pilot directory.
