# Commands

Validation and operating commands for this repository.

## How To Use This File

- Prefer real copyable commands.
- Keep the smallest reliable validation commands near the top.
- Update this file when the validation path changes.

## Documentation

Run from this example's root (`examples/web-app-with-auth` in the source checkout):

```bash
harnesskit context --current --json
harnesskit query "auth session" --limit 5 --json
harnesskit context docs/design-docs/auth.md --json
harnesskit check --json
harnesskit history snapshot -m "Review auth documentation"
```

From the HarnessKit source root, put query terms before the example target:

```bash
harnesskit query "auth session" examples/web-app-with-auth --json
```

Check refreshes the index automatically. Use `check --all` for every finding;
the default is a summary with baseline differences. This is a documentation
example; a clean check does not validate an application's authentication code.
Local usage logging is enabled by default; `HARNESSKIT_USAGE_LOG=off` disables it.

## Setup

## Run

## Test

## Lint

## Build

## Release
