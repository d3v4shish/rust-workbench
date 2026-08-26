# Rust Ownership Coach stress lab

This workspace intentionally does not compile. Each file isolates a realistic ownership shape so
Rust Workbench can be checked on code it did not use as an implementation fixture.

## Start here: the realistic application

`crates/commerce_app` is a multi-module commerce service rather than a collection of tiny
examples. It has domain models, a product catalog, checkout, caching, analytics, fulfillment,
messaging, and reports. Rustc currently reports **35 intentional errors: exactly five in each of
seven application modules**. `domain.rs` and most of `main.rs` are valid supporting code, so
navigation and cross-file analysis behave like an ordinary project.

Open `crates/commerce_app/src/catalog.rs` first. Its three functions demonstrate three different
borrow conflicts in one stateful component. Then use the following audit map:

| Application module | Errors | Scenario |
|---|---:|---|
| `catalog.rs` | 5 | E0502 ×3, E0499 ×2: map and vector borrow conflicts |
| `checkout.rs` | 5 | E0382 ×4, E0506: whole/conditional/partial moves and assignment while viewed |
| `cache.rs` | 5 | E0594 ×2, E0502 ×2, E0499: direct `Rc` mutation and collection conflicts |
| `analytics.rs` | 5 | E0596 ×2, E0506, E0502, E0382: receiver mutation, borrows, and a move |
| `fulfillment.rs` | 5 | E0382 ×3, E0505, E0506: partial moves, move while viewed, reassignment |
| `messaging.rs` | 5 | E0596 ×2, E0502 ×2, E0382: receiver mutation, queue invalidation, and a repeated move |
| `reports.rs` | 5 | E0382 ×3, E0502, E0506: partial/repeated moves and mutations while viewed |

`unsupported_cases` now supplies eight broader learning diagnostics: type mismatch, two invalid
returned references, a thread-safety trait requirement, method resolution, await outside async,
recursive async, and a closure that may outlive its borrow. The name is retained so existing test
scripts keep working, but these cases are intentionally supported by the broader Learning Debugger.

## How to test

1. If prompted, click **Trust this worktree**, then wait for Cargo check to finish.
2. Open `crates/commerce_app/src/catalog.rs` and put the cursor on a red-underlined expression.
3. The red **Explainable Rust issue detected** banner should appear for the supported errors above.
4. Press `Ctrl+Alt+O` or click **Explain visually**. Select **Visualize** to open the focused,
   stepped ownership diagram.
5. Confirm the panel says **Issue 1 of 5** and names both the Rust error code and binding.
6. Click **Next** and **Previous**; the selected issue and source highlight should move in source order.
7. Click `current` in `analytics.rs`; the panel should automatically select its E0506 issue. Click
   `event` in `record_then_log`; it should select E0382 without closing the panel.
8. For `redact_latest`, **Where the value went** must identify the source names and source lines.
   The pointer map must connect `prefix` to `*current` and then to `self.events`. `current`,
   `*current`, and `self` remain alive; only replacement/write access is temporarily blocked.
9. Verify the three large states say **Actual** for Before and Conflict. A selected-but-unapplied
   repair must say **Hypothetical**; only a fresh compiler check may produce **Verified result**.
10. Change diagram phases and click nodes; verify the diagram state changes without shifting its
    layout and Zed highlights the corresponding source lines.
11. Switch among the application modules and verify the panel follows the selected diagnostic.
12. Use the isolated repair packages in the second matrix when testing validated, applicable diffs.
13. In Fixes, inspect semantics and the diff before applying. Apply only in this disposable lab.
14. Press Undo after each application, or restore a file by reopening this generated workspace.

## Ownership Coach controls

- **Visualize** shows one focused box-and-arrow canvas. Solid facts come from the compiler or
  resolved source types; dashed yellow effects are explicitly possible or conceptual. Diagnostic
  phases move through Before, Conflict, and After while valid async and closure examples use their
  natural execution phases.

- **Where the value went** follows the selected compiler diagnostic through binding, transfer,
  borrow, rejected use, reinitialization, and drop. Each card separates source state, destination
  state, and allocation effect.
- **Pointers and ownership right now** draws the owner, reference handle, and referent separately. Color and
  symbols both encode state, so meaning does not depend on color alone. A borrowed value stays
  alive; only incompatible access is blocked.
- **Where this sits in the codebase** shows bounded source breadcrumbs, resolved local call paths,
  and compiler-known types without indexing work on the UI thread.
- **What the involved operations require** explains receiver access and effects. For example,
  `Vec::clear` requires `&mut self` because it drops every element and sets the vector length to
  zero; alternatives are listed only when their behavior is materially different.
- **Repair choices** separates compiler-validated edits from prewritten design alternatives.
  Preview `Rc`, `RefCell`, `Rc<RefCell<_>>`, or thread-safe topologies to see the counterfactual
  memory model and runtime tradeoffs before touching source. Applied edits remain one-step undoable.
- **Advanced compiler evidence** is the only collapsed secondary section. It contains exact MIR
  facts, loan endpoints, representation layers, and operation provenance.
- Use **A−**, the percentage button, and **A+** in the panel header to reduce, reset, or increase
  panel text. Display profiles still control which editor inlays appear independently from the
  sidebar explanation.
- `Ctrl+Alt+R` refreshes the current model after editing. Previous/Next walks all supported errors
  in the current file; moving the cursor onto another underlined error also changes selection.

## Detection matrix

| File | Expected rustc code | Expected coach category | Repair expectation |
|---|---:|---|---|
| `crates/repair_rc/src/main.rs` | E0382 | use after move | Rc and Arc candidates |
| `crates/move_cases/src/string_to_function.rs` | E0382 | use after move | ordinary clone/borrow may exist; wrapper not guaranteed |
| `crates/move_cases/src/partial_profile.rs` | E0382 | partial move | whole value rejected; unaffected field remains usable |
| `crates/move_cases/src/conditional_move.rs` | E0382 | use after conditional move | no wrapper guaranteed |
| `crates/move_cases/src/closure_capture.rs` | E0382 | use after move | no wrapper guaranteed |
| `crates/borrow_cases/src/two_writers.rs` | E0499 | multiple mutable borrows | lifetime explanation; no wrapper guaranteed |
| `crates/borrow_cases/src/read_then_write.rs` | E0502 | mutable while shared | lifetime explanation |
| `crates/borrow_cases/src/move_while_viewed.rs` | E0505 | move while borrowed | lifetime explanation |
| `crates/borrow_cases/src/assign_while_viewed.rs` | E0506 | assign while borrowed | lifetime explanation |
| `crates/borrow_cases/src/vec_reallocation.rs` | E0502 | mutable while shared | explains why a vector element reference blocks push |
| `crates/repair_refcell/src/main.rs` | E0596 | immutable mutation | RefCell, Mutex, and RwLock candidates |
| `crates/repair_rc_refcell/src/main.rs` | E0596 | immutable mutation | Rc+RefCell and thread-safe candidates |
| `crates/mutation_cases/src/shared_reference.rs` | E0596 | immutable mutation | shows immutable reference boundary |
| `crates/mutation_cases/src/rc_without_cell.rs` | E0596 | immutable mutation | demonstrates why Rc alone is not mutable |
| `crates/unsupported_cases/src/lib.rs` | E0277/E0308/E0373/E0515/E0599/E0728/E0733 | trait/type/lifetime/method/closure/async lessons | prewritten intent choices; standard editor actions remain available |

## Correct-code controls

Everything under `valid_patterns/src` is intended to compile. Use these files to ensure the coach
does not invent errors. Put the cursor on a value, open the panel with `Ctrl+Alt+O`, press
`Ctrl+Alt+R`, and inspect compiler facts without expecting a red problem banner.

- `nll.rs`: a shared borrow ends at its last use.
- `partial_reinitialize.rs`: a moved field is put back before using the whole struct.
- `rc.rs`: single-thread shared immutable ownership.
- `refcell.rs`: single-owner runtime-checked mutation.
- `rc_refcell.rs`: single-thread shared mutable ownership.
- `arc_mutex.rs`: cross-thread shared synchronized mutation.
- `rwlock.rs`: multiple readers or one writer across threads.
- `diagram_shapes.rs`: nested wrappers, `Cow`, closures, `&dyn Trait`, `Box<dyn Trait>`, pinned
  futures, and a compiler-valid Vec reallocation repair.

The hard test is not whether every file gets a suggestion. Correct behavior includes showing a
precise explanation with no repair, and showing no coach banner for unsupported diagnostic codes.

The repair cases are separate Cargo packages on purpose: each candidate must be able to recompile
without being rejected merely because another intentional error remains in the same package. The
large commerce package tests discovery, navigation, and explanations across many simultaneous
errors; the small packages test whether a proposed source rewrite independently compiles.
