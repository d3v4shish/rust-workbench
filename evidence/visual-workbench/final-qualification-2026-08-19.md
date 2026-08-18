# Rust Workbench final qualification — 2026-08-19

## Revisions and safety

- Baseline: `c5295eda49310c240b4a5a7d18b273148d576eb0`
- Rollback tag: `baseline/pre-integration` ->
  `f0d00af356913a93f82a19f2efc022a0634972f2`
- Qualified implementation: `d5853abfbe190b0866a949ac570c35c0e8f1ae5b`
- Branch: `feature/visual-ownership-sidebar`
- Remote: none configured; nothing was pushed.

## Correctness gates

| Gate | Result | Evidence |
| --- | --- | --- |
| `./workbench doctor` | Pass | Coherent stage1 rustc, rustdoc, std, proc-macro server, custom analyzer, and release editor found. |
| `./workbench test quick` | Pass, 10 s | 33 analyzer + 2 wrapper + 2 learning + 6 IDE + 29 workbench tests; stress fixture and touched Zed crates passed. |
| `./workbench test full` | Pass, 736 s | Includes quick gates, compiler run-make, coherent stage1 rebuild, release editor, compiler/analyzer benchmarks, exact-target checks, and wrapper validation. |
| Compiler run-make | Pass | `borrowck-autofix` selected suite: 1 passed, 0 failed. |
| Exact `self.events.push` target | Pass | Selected place `self.events`; operation `push`; access source `&self`; mutable borrow required; compiler-validated receiver repair. |
| Move wrapper repair | Pass | `Rc` and `Arc` candidates compiler-validated for the move fixture. |
| `git diff --check` | Pass | No whitespace errors. |
| JSON evidence validation | Pass | Both final performance evidence files parse successfully. |

The source-anchor mechanics protocol is version 3. Layout, storage, access, and
wrapper segments at one position are transported as one semantic hint. Zed
filters segments from structured metadata, so category toggles do not parse or
rewrite arbitrary visible prose.

## Performance gates

Three final `./workbench test performance` runs used the qualified compiler and
analyzer:

| Run | Compiler model median / p95 | Analyzer request median / p95 | Peak analyzer tree RSS | Event-loop stalls |
| --- | ---: | ---: | ---: | ---: |
| 1 | 548.110 / 555.768 ms | 1.107 / 1.413 ms | 589,984 KiB | 0 |
| 2 | 556.919 / 557.553 ms | 1.119 / 1.384 ms | 590,016 KiB | 0 |
| 3 | 550.831 / 578.282 ms | 1.063 / 1.242 ms | 587,376 KiB | 0 |

The middle compiler medians/p95 values are 16.687%/18.577% faster than the
saved baseline. The middle analyzer median/p95 values are 20.759%/60.823%
faster. The largest ownership artifact is 2,438,302 bytes, below the 8 MiB
gate.

The real merged-metadata inline path was run three times with 128 hints per
batch: 74,511 ns, 75,333 ns, and 72,107 ns p95. The worst result is 0.075 ms,
well below the 4 ms gate. Three UI-model runs also remained below every scene,
full-map, scrubber, cached-switch, and preview-swap threshold; see
`ui-performance-2026-08-18.json`.

A 16-minute stress-project session plateaued: editor RSS samples were 372, 375,
and 296 MiB; final combined editor/analyzer/proc-macro RSS was 372.6 MiB. There
were no ownership-artifact stalls over 100 ms. Two logged stalls were bounded
one-time startup cache work (146.804 and 114.660 ms), not panel interaction.

## Packaging

- Command: `./workbench package linux --skip-build`
- Archive:
  `dist/rust-workbench-linux-x86_64-glibc2.43.tar.zst`
- Size: 502,598,753 bytes (479.3 MiB)
- SHA-256:
  `893da66a038a8c8f1eaed3030bb159fda16afcb6cf43d36bcb0a97705cbfd4d9`
- Target: Linux x86_64, glibc >= 2.43
- Staging bundle doctor/smoke: pass
- Relocated archive doctor/smoke from a path containing spaces: pass
- Explicit `./workbench test bundle --archive ...`: pass

The relocated smoke test ran bundled Cargo/rustc on both a valid native-linking
crate and an intentionally broken crate that emits JSON diagnostics. Dynamic
dependency closure checks passed for the editor, analyzer, Cargo, rustc, native
compiler, and bundled shared libraries.

The optional clean-container gate was attempted, but the host did not grant
access to `/var/run/docker.sock`. It is explicitly conditional in the release
plan; native staging and relocated-archive isolation gates passed instead.

## Implementation equivalences and bounds

- The UI uses single-entry compact/full/preview derived-scene caches. This is a
  stricter memory bound than the planned maxima of 32/8/8 and avoids retaining
  stale issue graphs.
- Component boundaries are focused pure derivation/render functions within the
  workbench crate rather than one filesystem module per card. Tests cover the
  same state boundaries without increasing the public crate surface.
- Deterministic structural assertions are used for graph snapshots. They check
  stable IDs, node/edge meaning, ordering, provenance, prose, and transitions
  without brittle pixel-golden files.
- Runtime-only values such as addresses, allocation counts, capacities, lock
  state, reference counts, and borrow flags remain `unknown` unless runtime
  evidence exists.

## Documentation and manual matrix

The complete workflow, settings, provenance rules, inline clue categories,
keyboard interaction, panel sizes, font scales, themes, stress fixtures,
expected exact targets, limitations, and performance observations are in
`zed-rust/docs/src/languages/rust-learning-debugger.md`. Prettier 3.6.2 write
and check both passed.
