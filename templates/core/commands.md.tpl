# Commands

Validation and operating commands for this repository.

## How To Use This File

- Prefer real copyable commands.
- Keep the smallest reliable validation commands near the top.
- Update this file when the validation path changes.

## Documentation

Run from the project root; query terms precede an optional target directory.

```bash
harnesskit context --current --json
harnesskit query "distinctive keywords" --limit 5 --json
harnesskit check --json
harnesskit history snapshot -m "Describe the documentation change"
```

Check refreshes the index automatically. Use `check --all` for all findings;
the default is a summary with baseline differences. A clean docs check does not
replace the project's tests or acceptance checks. Commands log local usage by
default; `HARNESSKIT_USAGE_LOG=off` disables logging.

## Setup

## Run

## Test

## Lint

## Build

## Release
