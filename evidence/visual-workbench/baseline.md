# Visual ownership baseline

Captured on 2026-08-18 from commit
`c5295eda49310c240b4a5a7d18b273148d576eb0` on branch
`feature/visual-ownership-sidebar`. The local rollback tag is
`baseline/pre-integration`. No remote object was created.

## Correctness

| Command | Result | Elapsed | Important coverage |
| --- | --- | ---: | --- |
| `./workbench doctor` | Passed | under 1 s | Toolchains, source trees, custom rustc, cargo, rust-analyzer, and editor found |
| `./workbench test quick` | Passed | 506 s | 31 analyzer ownership tests, wrapper and learning tests, 6 IDE insight tests, 26 Zed workbench tests, stress lab, integration crate |
| `./workbench test full` | Passed | 669 s | Quick suite, compiler run-make suite, release editor build, correctness and performance fixtures |

Three existing dead-code warnings were emitted for `distance_to_range`,
`problem_distance`, and `nearest_ownership_problem_index` in
`rust_workbench.rs`. They predate schema-6 work and are preserved as baseline
behavior.

The stress-lab gate observed at least 40 diagnostics and at least five
unsupported errors. Its existing source comments preserve the intended
`self.events`, `current`, `prefix`, move, partial-move, and wrapper cases.

## Performance and memory

`./workbench test performance` passed three consecutive times. The full raw
measurement set is stored in
[`baseline-performance.json`](baseline-performance.json). Across those three
standalone runs:

- compiler baseline median: 362.060–441.640 ms;
- compiler ownership-model median: 657.819–763.658 ms;
- ownership request median: 1.307–1.422 ms;
- ownership request p95: 2.579–4.683 ms;
- analyzer peak RSS: 605,784–611,708 KiB;
- ownership-specific event-loop stalls: zero;
- largest ownership artifact: 1,290,521 bytes.

The full-suite `self.events` fixture selected the exact `self.events` place and
reported a 6.906 ms median and 8.020 ms p95 with zero ownership stalls. The
wrapper-repair fixture validated both `Rc` and `Arc` candidates.

`./workbench disk` passed. The largest local consumers were the Zed release
target (31.6 GiB), rustc build tree (26.2 GiB), and Zed debug target (17.3 GiB).
There was no live editor/analyzer process during that command; consequently a
live editor RSS baseline is deliberately not claimed. The analyzer RSS above
comes from the dedicated benchmark process. No existing issue-switch latency
harness was found, so the UI pass must add one before measuring that metric.

## Baseline interpretation

The compiler transport measurements vary enough that a final 5% comparison
must use repeated samples and report raw values rather than a single run. The
interactive analyzer request path is stable at low single-digit milliseconds,
which is the relevant smoothness baseline for the sidebar. Existing logs
recorded zero ownership-specific `overly long loop turn` events.
