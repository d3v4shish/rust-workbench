# Workspace ownership guide qualification — 2026-08-20

Source base: `45611354f0` plus the local, uncommitted Rust Workbench changes.
No remote objects were created or pushed.

## Correctness

- `cargo +1.97.1 check -p rust-analyzer`: passed.
- `RUN_SLOW_TESTS=1 cargo +1.97.1 test -p rust-analyzer --test slow-tests ownership_workspace_guide_tracks_roots_without_name_based_cross_file_grouping -- --nocapture`: passed in 0.41 s.
- `cargo +1.97.1 test -p rust-analyzer ownership_wrapper_preview --lib`: 4 passed.
- `cargo +1.97.1 test -p rust_workbench --manifest-path zed-rust/Cargo.toml`: 36 passed.
- `./workbench doctor`: passed.
- `./workbench test quick`: passed in 131 s.
- `./workbench test full`: passed in 803 s, including compiler run-make,
  stage1 analyzer, release editor, three performance scenarios, and validated
  wrapper repair checks.

The multi-file LSP fixture proves that local diagnostics from `analytics.rs`
and `reports.rs` appear in one workspace response, while same-named `values`
locals remain separate clusters. The repeated request preserves both the
revision and selected root. Zed tests cover server-selected roots, intent-based
repair ranking, stale-model rejection, exact field targeting, and fail-closed
repair application.

## Release build

- `./workbench build analyzer`: passed.
- `./workbench build editor`: passed; optimized Zed build completed in 9 min 14 s.

These are the release artifacts selected by `./workbench run`, not only debug
test binaries.

## Performance

Final `./workbench test performance` results using the rebuilt release analyzer:

| Measurement | Result | Gate |
| --- | ---: | ---: |
| Ownership model median | 1.184 ms | 20 ms p95 gate |
| Ownership model p95 | 1.525 ms | 20 ms |
| Ownership model max | 1.844 ms | 100 ms |
| Workspace guide median | 1.241 ms | 20 ms p95 gate |
| Workspace guide p95 | 1.813 ms | 20 ms |
| Workspace guide max | 2.297 ms | 100 ms |
| Workspace guide clusters | 52 | capped at 100 |
| Workspace guide median response | 115,160 bytes | bounded protocol |
| Analyzer peak RSS | 591,160 KiB | observation |
| Ownership event-loop stalls | 0 | must be zero |

The ownership model median was 15.217% faster and p95 was 56.840% faster than
the saved baseline samples. The workspace-guide request is now part of the
standard performance gate and executes 100 warm iterations.
