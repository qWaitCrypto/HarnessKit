# HarnessKit Agent Surfaces

HarnessKit integrates with coding agents through skills plus the local CLI.

This directory is runtime-facing packaging material, not product discussion notes.

## Shape

```text
harnesskit CLI
+ one shared HarnessKit skill
+ direct Codex skill installation
+ optional thin Codex plugin packaging
+ thin Claude Code installation path
```

HarnessKit does not provide an MCP server, daemon, tool wrapper, or large slash-command menu. The CLI is the deterministic backend. The skill is the agent-facing workflow.

## Expected Install Model

- Public install: installs the `harnesskit` CLI and installs the agent skill for Claude Code and Codex.
- Manual skill install: installs only the skill; the CLI must be installed separately.
- Optional Codex plugin install: packages the same skill for Codex marketplace/plugin testing.

The exact installer can evolve later. The surface contract should stay small: the skill assumes `harnesskit` is available on `PATH`, and otherwise reports that deterministic actions require the CLI.

The skill content is shared at:

```text
agent-surfaces/skills/harnesskit/SKILL.md
```

Codex and Claude Code should both install or package this same skill. Do not maintain separate duplicated skill bodies for each agent.

For Codex direct skill installation, copy the shared skill to:

```text
$CODEX_HOME/skills/harnesskit/SKILL.md
```

or, when `CODEX_HOME` is unset:

```text
~/.codex/skills/harnesskit/SKILL.md
```

For optional Codex plugin packaging, generate a copy of the shared skill into the plugin package as:

```text
agent-surfaces/codex/skills/harnesskit/SKILL.md
```

That copied file is a build/install artifact and should not be maintained as source. The source of truth remains `agent-surfaces/skills/harnesskit/SKILL.md`.

Use:

```bash
scripts/sync-agent-surfaces.sh
```

before validating or packaging the Codex plugin.

## Init Behavior

After installation, either the user or the agent can run:

```bash
harnesskit init
```

When an agent runs init, it should:

1. Create the managed docs scaffold through the CLI.
2. Explain any local host Git exclude update reported by the CLI.
3. Prefer `.git/info/exclude` over `.gitignore` for local isolation.
4. Read the newly created agent entrypoint in the same session:
   - Codex: `AGENTS.md`
   - Claude Code: `CLAUDE.md`
5. Continue by following `ARCHITECTURE.md` -> `docs/index.md` -> collection index -> focused doc.

## Permanent Non-Goals

- MCP server
- long-running daemon
- custom agent runtime
- wrapper tools for every CLI command
- many user-facing slash commands

The purpose is to make agents proactively use file-first project memory, not to create a second runtime around them.
