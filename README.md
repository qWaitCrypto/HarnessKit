# HarnessKit

HarnessKit is in active development: the core local CLI is usable, Phase 2 indexing is treated as complete, and the agent surface is being kept deliberately small.

Contributions, critiques, strange edge cases, and better names are welcome.

## Core Principles

- Highest principle: stay aligned with the OpenAI harness idea.
- Second principle: stay lightweight.
- Keep product artifacts separate from discussion artifacts.

## Current Focus

HarnessKit is being discussed as a local-first repo context harness for coding agents.

The working idea is not to build another agent runtime. Codex, Claude Code, Cursor, and similar tools already own the agent loop, tool execution, sandboxing, and conversation lifecycle. HarnessKit should focus on the external project context layer: indexing repo knowledge, producing evidence-backed context packets, preserving task checkpoints, helping resume after context loss, and reporting docs/code drift.

Implementation now includes a Rust CLI for initialization, indexing, checks, query/context retrieval, graph inspection, and local docs history. The current Phase 3 surface direction is shared skill + CLI: no MCP server, no daemon, and no large command menu.

## Documents

- [Product discussion](docs/prd-repo-context-harness.md) - current long-form product direction.
- [Agent surface](docs/agent-surface-cli-v0.md) - current Phase 3 decision: shared skill + local CLI.
- [Local alpha install](docs/local-alpha-install.md) - current local CLI/Claude/Codex install path for testing.
- [Docs index](docs/README.md) - document map and status.
