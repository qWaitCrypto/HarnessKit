# Weave W0 corpus

This directory stores expectations, not a checked-in Git repository. All source,
documents and commits are synthetic. Git and Python 3 are test prerequisites;
Rust/TypeScript/Python/Go toolchains are not needed to generate the source files.
No dependencies or production CLI behavior are added by W0.

Run the evaluation skeleton from the HarnessKit root:

```bash
python3 scripts/evaluate-weave.py
```

The runner generates history inside `TemporaryDirectory`, validates the corpus
against its tagged revisions, removes the repository, and emits JSON. All 37
behavior cases are explicitly `pending`, with their required W steps listed;
`executed` is zero and there are no quality scores. A successful exit means the
fixture inputs are valid, not that any Weave feature passes. W3/W4/W5 will
connect the remaining evaluators; W9 will enforce the design's metric thresholds. The
existing S5 corpus and regression gates remain independent and unchanged.

`tests/anchors.rs` connects the W2 prerequisites to the existing corpus: all eight
retrieval cases must have real section-to-symbol EXTRACTED edges, and all twenty
drift cases must resolve their anchors before each change. At the after revision,
logic/format/review changes keep the edge; moves/deletions produce W2 info
diagnostics. These assertions do not claim retrieval ranking or W3 drift behavior,
so the full feature evaluations remain pending in the skeleton runner.

For manual inspection, generate only in a disposable directory:

```bash
weave_fixture_dir="$(mktemp -d)"
python3 scripts/generate-weave-fixture.py "$weave_fixture_dir"
git -C "$weave_fixture_dir" log --reverse --oneline --decorate
```

The generator refuses nonempty destinations and prints a JSON manifest mapping
labels to commit IDs. It fixes both identities, author/committer timestamps,
timezone, branch (`main`), object format (SHA-1), messages, tags, UTF-8 bytes and
LF endings. Caller `GIT_*` overrides, global/system configuration, templates,
hooks, signing, filters and excludes cannot change the history. Only tracked
trees and commit IDs are reproducibility inputs; Git index mtimes are not.

| Tag | Change |
| --- | --- |
| `baseline` | Four languages, decisions and experience notes, explicit anchors and unrelated bulk files. More than 30 files, so the initial import must not create cochange edges. |
| `logic` | Change each primary function's offset from 60 to 90; documents stay at the old contract. |
| `format` | Change indentation and add a comment to each primary function; preserve its logic. |
| `moved` | Move one unchanged symbol per language into another file, leaving its document anchor untouched. |
| `deleted` | Remove a separate symbol per language, leaving its anchor untouched. |
| `reviewed-rust`, `reviewed-typescript`, `reviewed-python`, `reviewed-go` | Four separate commits, each changing one source file plus its decision and experience document. Primary offsets become 120. |
| `bulk` | Change 33 files, including a document and an unrelated Rust file, to provide a negative cochange control. |

The fixture tracks a version-0 schema using existing `include` rules, since
`docs/notes/` is not a default alpha.7 collection. Derived `.harnesskit` state is
ignored, while the schema remains tracked. No `knowledge.lock` or expected
structural fingerprints are generated: these belong to later steps.

`cases.json` has `schema_version: 1`, `fixture: "weave"`, a fixed `as_of` UTC clock
for future time-decay evaluation, and three case arrays with globally unique IDs:

| Array | Fields and expectation semantics |
| --- | --- |
| `retrieval` | `revision`, `query`, `top_k`, `expected_sections: [{path, heading}]`, `expected_symbols: ["path#symbol"]`. Score document sections and symbols separately; each target should appear within its side's top-k. Includes Chinese queries for all four languages and exact identifiers. |
| `drift` | `before`, `after`, `doc`, `anchor: "path#symbol"`, `expected_rule`, and `expected_target` for moves. Establish a fresh seal at `before`, then update to `after`. A null rule means no drift warning for that anchor, including formatting and automatic review by a joint doc/code commit; other anchors are outside that case's assertion. |
| `cochange` | `revision`, `doc`, `top_k`, `expected_paths`, `forbidden_paths`. Known pairs should enter the document's top eight cochange edges; forbidden pairs should be absent. Empty expected paths support negative controls. Evaluate with the top-level `as_of` clock. |

Paths are relative to the generated root. Revisions are the lightweight tags
above. The runner checks IDs, revision/file/heading/symbol target presence and
conflicting cochange expectations. `tests/weave_fixture.rs` additionally checks
history reproducibility under hostile Git settings, nonempty destination safety,
the scripted changes, bulk exclusion controls, pending reporting, invalid inputs,
and document indexing/search through the existing CLI. These are fixture tests,
not implementations of symbol extraction, structural hashing or graph scoring.
