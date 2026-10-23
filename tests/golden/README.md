# W5b golden output set

`w5b-cases.json` contains ten fixed query/context/check invocations against the
deterministic W0 fixture. `golden_output.rs` computes signal density and
metadata character share for every case and checks the first line against the
small snapshot files here. When output intentionally changes, update the
snapshot and the test in the same change; do not refresh snapshots implicitly.
