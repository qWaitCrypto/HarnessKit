# Weave output gates

`w5b-cases.json` keeps the original ten invocations and adds seven queries plus
changed-diff, drift-check and both edit-hook outputs: twenty-one complete snapshots,
including eleven fixed queries. Repositories and Git history are disposable W0
fixtures; the sparse repository fixture also has real handoff entrypoints.

Run `python3 scripts/evaluate-output.py` (or `cargo test --test golden_output`).
Every case must have signal density >= 70% and path/badge character share <= 35%.
The metric masks complete paths and badges before recognizing query words,
signatures and prose; negative controls reject metadata-only lines even when
paths contain the query. Both numerator and denominator are reported per case.

Snapshots compare the entire output. Only the temporary root and the wall-clock
time in working-tree drift notices are normalized. The runner never updates
snapshots. Review complete text differences before editing an expected file.

Run `cargo build --release` followed by `python3 scripts/benchmark-weave.py` for
the separate 10,000-file performance gate. It measures CLI wall time, including
startup and automatic indexing, takes the slowest of three samples, and uses
distinct exact-symbol code queries that cannot reuse a result-cache entry.
Cold indexing removes derived state; it does not flush operating-system caches.
