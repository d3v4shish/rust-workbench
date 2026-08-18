# Rust Workbench Visual Learning TODO

This tracker turns the approved diagram-first Rust learning plan into executable
work. It covers the custom compiler, rust-analyzer, Zed editor integration,
sidebar UI, inline clues, correctness, performance, documentation, and
packaging.

## How to use this file

- Keep every task and acceptance checkbox unchecked until its evidence exists.
- Complete milestones in order unless a task explicitly has no dependency.
- Do not mark a UI task complete from a screenshot alone. Its model and
  interaction tests must also pass.
- Do not mark an analyzer or compiler task complete from unit tests alone. Run
  the relevant end-to-end stress-lab scenario.
- Record command output, benchmark JSON, screenshots, or a commit hash in the
  evidence log before checking a milestone's final gate.
- `c5295eda4` is the correctness and performance baseline.
- `baseline/pre-integration` is the rollback tag.
- Keep all work local. Do not push to GitHub.

### Priority legend

- **P0:** Required for the first complete visual-learning release.
- **P1:** Required before final packaging, but may follow the main UI work.
- **P2:** Recommended next-phase learning feature; not part of the first release.

### Completion rule

A milestone is complete only when:

- [x] Every P0/P1 implementation task in that milestone is checked.
- [x] Every acceptance criterion is checked.
- [x] The listed automated tests pass.
- [x] Required manual checks have recorded evidence.
- [x] No unrelated user changes were modified or removed.

## Milestone 0: Safety, baseline, and measurement

### BASE-001 — Confirm the rollback point [P0]

- [x] Confirm `git rev-parse HEAD` is based on `c5295eda4` before feature work.
- [x] Confirm `baseline/pre-integration` resolves to the intended rollback
      commit.
- [x] Create a local feature branch for the visual ownership work if one is not
      already checked out.
- [x] Confirm `git status --short` contains no unexplained changes.

Acceptance criteria:

- [x] The baseline commit and tag resolve successfully.
- [x] Existing user changes, if any, are documented and preserved.
- [x] No remote branch or GitHub object is created.

### BASE-002 — Capture baseline correctness [P0]

- [x] Run `./workbench doctor`.
- [x] Run `./workbench test quick`.
- [x] Run `./workbench test full` before changing compiler or analyzer schemas.
- [x] Save the pass/fail result and elapsed time in the evidence log.

Acceptance criteria:

- [x] The existing compiler, analyzer, editor, and stress-lab gates pass.
- [x] Any pre-existing failure is reproduced, explained, and isolated before
      implementation begins.

### BASE-003 — Capture baseline performance and memory [P0]

- [x] Run `./workbench test performance` at least three times.
- [x] Save raw JSON for baseline/model median and p95 timings.
- [x] Run `./workbench disk` with the editor and analyzer open on the stress lab.
- [x] Record editor RSS, analyzer RSS, artifact bytes, and event-loop warnings.
- [x] Record sidebar issue-switch latency if an existing harness supports it.

Acceptance criteria:

- [x] Results are tied to commit `c5295eda4` and the machine configuration.
- [x] The measurements are repeatable enough to evaluate a 5% regression.
- [x] Baseline logs include any existing `overly long loop turn` messages.

### BASE-004 — Lock critical correctness fixtures [P0]

- [x] Preserve the `analytics.rs` cases that select `self.events`, `current`, and
      `prefix`.
- [x] Preserve the move, partial-move, wrapper-repair, and stress-lab fixtures.
- [x] Add source comments identifying the intended diagnostic in each fixture
      without changing its behavior.

Acceptance criteria:

- [x] `analytics.rs` contains at least five compiler errors in the same file.
- [x] The current analyzer returns `self.events` for the E0596 mutation case.
- [x] The current analyzer returns both `prefix` and `*current` conflict nodes
      for the borrowed-current case.

## Milestone 1: Compiler ownership artifact schema v6

Depends on: Milestone 0.

### COMP-001 — Define the version-6 graph contract [P0]

- [x] Bump `BORROWCK_OWNERSHIP_MODEL_SCHEMA_VERSION` from 5 to 6.
- [x] Add serialized memory graph, node, edge, snapshot, access-path, and access
      step types.
- [x] Add stable serialized enums for storage region, node kind, edge relation,
      access kind, ownership state, and provenance.
- [x] Add `#[serde(default)]` or optional fields wherever older consumers must
      remain compatible.
- [x] Document source-level storage semantics separately from optimized physical
      placement.

Acceptance criteria:

- [x] A schema-6 artifact round-trips through serialization tests.
- [x] The artifact contains no duplicated source file text.
- [x] Unknown enum data fails safely or maps to an explicit `unknown` value.
- [x] Artifact consumers can distinguish exact, derived, conceptual, and unknown
      facts.

### COMP-002 — Emit stable memory nodes [P0]

- [x] Emit one stable node for each relevant local binding and exact MIR place.
- [x] Emit separate nodes for handles, inline wrapper state, heap allocations,
      buffers, control blocks, guards, and borrowed views.
- [x] Attach type, size, alignment, source range, body ID, place, state, and
      provenance.
- [x] Use stable IDs derived from body/place/layer identity rather than vector
      position.
- [x] Cap recursive topology depth at 12 and mark truncation explicitly.

Acceptance criteria:

- [x] Reordering unrelated bindings does not change IDs for unchanged places.
- [x] A local `Vec<T>` has a distinct local header and runtime-sized buffer node.
- [x] A `Box<T>` has a local handle and a heap allocation for `T`.
- [x] Generic or unsized layouts are represented without fabricating a size.

### COMP-003 — Emit ownership and pointer edges [P0]

- [x] Emit `owns`, `contains`, `owns_buffer`, `points_to`, `borrow_shared`,
      `borrow_mutable`, `reborrow`, `shares_allocation`, `weak_reference`,
      `guards_access`, `conditional`, and `moved_to` relations where supported.
- [x] Attach the related event, loan, source range, and provenance.
- [x] Preserve projections such as fields, indexes, slices, and dereferences.
- [x] Represent reference-to-field relationships without collapsing them to the
      root local.

Acceptance criteria:

- [x] `prefix: &str` points to the relevant place inside `current`.
- [x] A move transfers an ownership relation to its destination.
- [x] `Rc::clone` and `Arc::clone` produce multiple handles sharing one
      allocation rather than multiple allocations.
- [x] A weak pointer targets the control block with a weak edge.

### COMP-004 — Emit ownership-relevant snapshots [P0]

- [x] Emit snapshots for initialization, borrow reservation, activation, move,
      copy, clone, reborrow, last use, borrow end, conflict, reinitialization,
      and drop.
- [x] Record source ranges and MIR locations.
- [x] Preserve branch/loop path markers so mutually exclusive paths are not
      displayed as a single execution.
- [x] Emit state deltas instead of duplicating the entire graph where practical.

Acceptance criteria:

- [x] The borrowed-current fixture shows borrow creation, rejected mutation,
      later reference use, and borrow end in the correct order.
- [x] A two-phase mutable borrow distinguishes reservation and activation.
- [x] A conditional move is labeled with its control-flow path.
- [x] Snapshot size remains bounded on the stress lab.

### COMP-005 — Emit typed access paths [P0]

- [x] Record built-in dereference, trait `Deref`, trait `DerefMut`, auto-borrow,
      explicit raw-pointer dereference, wrapper access, and guard dereference.
- [x] Record starting type, result type, mutability, explicitness, fallibility,
      panic risk, and unsafe requirement.
- [x] Represent `Weak::upgrade`, `Option`/`Result` extraction, and lock/borrow
      guard creation.

Acceptance criteria:

- [x] `Box<T>` reports built-in dereference to `T`.
- [x] `Rc<T>` reports shared dereference and does not claim general mutable
      dereference.
- [x] `RefCell<T>` reports that `borrow`/`borrow_mut` is required.
- [x] A raw pointer reports explicit unsafe dereference.

### COMP-006 — Add compiler schema tests [P0]

- [x] Add serialization tests for every new type and enum.
- [x] Add compiler tests for moves, copies, clones, partial moves, borrows,
      reborrows, and reinitialization.
- [x] Add nested-wrapper tests for `Rc<RefCell<Vec<T>>>` and
      `Arc<Mutex<Vec<T>>>`.
- [x] Add branch, loop, early-return, fat-pointer, raw-pointer, generic, unsized,
      zero-sized, and truncation tests.
- [x] Update artifact-size/performance scripts that currently require schema 5.

Acceptance criteria:

- [x] `cd rust && ./x test --force-rerun tests/run-make/borrowck-autofix`
      passes.
- [x] All new schema fixtures validate as version 6.
- [x] Existing schema-5 fixtures remain readable by downstream compatibility
      tests.

## Milestone 2: Compiler topology coverage

Depends on: COMP-001 through COMP-006.

### TOPO-001 — Model references and direct pointers [P0]

- [x] Model `&T`, `&mut T`, `&str`, slices, trait-object fat pointers, raw
      pointers, and `NonNull<T>`.
- [x] Separate the pointer/reference representation from its pointee.
- [x] Show metadata words for fat pointers conceptually without inventing
      runtime values.
- [x] Preserve exact field and element projection targets.

Acceptance criteria:

- [x] Shared and mutable references use distinct edges and states.
- [x] Slice and trait-object references are identified as fat pointers.
- [x] Raw pointers are not presented as owners or safe borrows.

### TOPO-002 — Model owning and shared smart pointers [P0]

- [x] Model `Box`, `Rc`, `rc::Weak`, `Arc`, `sync::Weak`, and `Pin<P>`.
- [x] Separate the `Rc`/`Arc` handle, control block, and inner value.
- [x] Represent pinning as a movement constraint, not a storage location.
- [x] Treat source-visible handles separately from runtime reference counts.

Acceptance criteria:

- [x] `let b = Rc::clone(&a)` produces two handles and one allocation.
- [x] Moving an `Rc` handle invalidates the source binding without duplicating
      the allocation.
- [x] `Pin<Box<T>>` shows both ownership and the pinning constraint.

### TOPO-003 — Model interior mutability and synchronization [P0]

- [x] Model `Cell`, `RefCell`, `UnsafeCell`, `Mutex`, `RwLock`, `OnceCell`, and
      `OnceLock`.
- [x] Add visual gate nodes for runtime borrow or synchronization checks.
- [x] Model returned guards as temporary values that provide dereference access.
- [x] Distinguish panic, fallible, blocking, and unsafe access behavior.

Acceptance criteria:

- [x] `Rc<RefCell<Vec<T>>>` renders recursively through every wrapper.
- [x] `Arc<Mutex<T>>` shows shared ownership followed by an exclusive lock gate.
- [x] No runtime lock or borrow state is claimed without runtime evidence.

### TOPO-004 — Model standard allocating containers [P1]

- [x] Model `Vec`, `String`, `VecDeque`, `BinaryHeap`, `PathBuf`, and `OsString`.
- [x] Model `HashMap`, `HashSet`, `BTreeMap`, `BTreeSet`, and `LinkedList` using
      standard-library conceptual allocation contracts.
- [x] Model boxed slices and boxed strings.
- [x] Mark runtime length, capacity, buckets, and node counts as unknown.

Acceptance criteria:

- [x] Every container separates its fixed-size local representation from
      runtime allocation where applicable.
- [x] Conceptual collection internals are labeled as standard-library semantics,
      not exact physical layout.
- [x] No std-version-specific private field layout is exposed as a stable fact.

### TOPO-005 — Model aggregates and conditional ownership [P1]

- [x] Model arrays, tuples, structs, unions, enums, `Option`, `Result`, and `Cow`.
- [x] Show exact inline layout and field containment where available.
- [x] Show borrowed/owned alternatives for `Cow` as conditional branches.
- [x] Treat custom wrappers as opaque unless ownership can be proven.

Acceptance criteria:

- [x] `Option<Box<T>>` shows conditional ownership without asserting the active
      variant.
- [x] A custom `MyBox<T>` does not become an owning pointer based on its name.
- [x] Struct padding and field offsets match target layout data.

## Milestone 3: Rust-analyzer ingestion and fallback model

Depends on: Milestones 1 and 2.

### RA-001 — Ingest schema 6 and preserve older schemas [P0]

- [x] Accept compiler artifact schemas 2 through 6.
- [x] Convert graph nodes, edges, snapshots, and access paths into analyzer model
      types.
- [x] Keep all new LSP fields optional and bump the extended response schema to
      12.
- [x] Preserve URI, source hash, artifact revision, selected problem ID, and
      exact place through every conversion.

Acceptance criteria:

- [x] Schema-6 models expose the full graph.
- [x] Schema 2–5 artifacts still return the best model available from their
      fields.
- [x] Unsupported future schemas fail with a clear status instead of crashing.

### RA-002 — Merge exact and estimated facts [P0]

- [x] Build a conservative analyzer-only scene before Cargo check completes.
- [x] Use type inference, method resolution, expression adjustments, reference
      search, and control-flow data.
- [x] Replace estimated facts with compiler-exact facts after a valid artifact
      arrives.
- [x] Never merge facts from different source hashes or problem IDs.

Acceptance criteria:

- [x] Estimated facts are visibly marked `analyzer estimate`.
- [x] Saving/checking upgrades the same selected problem without changing its
      exact target.
- [x] Stale responses cannot replace a newer scene.

### RA-003 — Produce layout and storage facts [P0]

- [x] Reuse rust-analyzer's target layout engine for locals, ADTs, fields,
      aliases, arrays, and tuples.
- [x] Report size, alignment, padding summary, field offset, zero-sized status,
      unsized status, and `Drop` requirement.
- [x] Separate handle size from runtime allocation size.
- [x] Attach target triple and provenance.

Acceptance criteria:

- [x] Struct layout matches existing memory-layout hover results.
- [x] Generic layouts say that size depends on type parameters.
- [x] `Vec<T>` never labels 24 bytes as the size of its heap buffer.

### RA-004 — Produce dereference and usability facts [P0]

- [x] Reuse expression-adjustment and method-resolution data for actual calls.
- [x] Produce capability facts for selected bindings even before a call exists.
- [x] Explain automatic versus explicit access and the resulting type.
- [x] Report fallibility, panic risk, poisoning, and unsafe requirements.

Acceptance criteria:

- [x] The model accurately distinguishes `Box`, `Rc`, `RefCell`, `Mutex`,
      `Weak`, `Option<Box<T>>`, and raw pointers.
- [x] Unresolved trait obligations return `unknown` rather than a false access
      path.

### RA-005 — Generalize method contracts [P0]

- [x] Return the resolved signature and `self`/`&self`/`&mut self` requirement
      for every resolved method.
- [x] Return argument ownership and return-borrow relationships.
- [x] Use local MIR/body facts when available.
- [x] Use documentation summaries separately from proven ownership effects.
- [x] Remove UI dependence on method-name-specific branches.

Acceptance criteria:

- [x] `Vec::clear`, `push`, and a user-defined mutable method use the same
      contract pipeline.
- [x] External methods without bodies still show signature-derived facts.
- [x] Related methods are labeled as discovery, not validated repairs.

### RA-006 — Extend repair validation with preview models [P0]

- [x] Allow a validated temporary repair to return an optional ownership model
      or graph delta for the patched source.
- [x] Key preview results by source hash and repair ID.
- [x] Cancel validation when source or selection changes.
- [x] Keep unvalidated candidates separate from compiler-validated previews.

Acceptance criteria:

- [x] Previewing does not modify the real source file.
- [x] A validated preview reports whether the selected diagnostic disappears.
- [x] Replacement diagnostics are reported rather than hidden.

### RA-007 — Add analyzer regression tests [P0]

- [x] Add tests for schema compatibility, exact-place retention, graph
      conversion, snapshots, layouts, access paths, method contracts, and repair
      previews.
- [x] Extend the learning-context benchmark assertions for graph nodes and
      access steps.

Acceptance criteria:

- [x] `cargo test --manifest-path rust/src/tools/rust-analyzer/Cargo.toml -p rust-analyzer ownership_ --lib --bins`
      passes.
- [x] `cargo test --manifest-path rust/src/tools/rust-analyzer/Cargo.toml -p rust-analyzer learning_problem --lib`
      passes.
- [x] `cargo test --manifest-path rust/src/tools/rust-analyzer/Cargo.toml -p ide ownership_insight --lib`
      passes.

## Milestone 4: Inline type-mechanics clues

Depends on: RA-003 through RA-005.

### HINT-001 — Add mechanics hint configuration [P0]

- [x] Add rust-analyzer configuration for mechanics master enable, layout,
      storage, access, and wrapper hints.
- [x] Keep the feature disabled by default in stock rust-analyzer.
- [x] Enable it through the custom Zed language configuration when the
      workbench requires it.
- [x] Trigger inlay refresh when settings change.

Acceptance criteria:

- [x] VS Code or another standard LSP client can enable each category through
      rust-analyzer settings.
- [x] A client that does not understand custom metadata still renders a normal
      inlay label.

### HINT-002 — Generate compact hint labels [P0]

- [x] Generate layout labels such as `16 B · align 8 · inline`.
- [x] Generate storage labels such as `24 B handle → heap buffer`.
- [x] Generate access labels such as `auto-deref Rc → T · shared`.
- [x] Generate wrapper labels such as `borrow_mut() → RefMut<T>`.
- [x] Merge categories at the same source anchor into one label.

Acceptance criteria:

- [x] At most one mechanics label appears at a given anchor.
- [x] Compact labels do not include unsupported runtime values.
- [x] Long types are shortened in the label and complete in the tooltip.

### HINT-003 — Add detailed lazy tooltips [P0]

- [x] Show the complete type/access chain.
- [x] Explain whether access is automatic, explicit, fallible, panicking,
      blocking, or unsafe.
- [x] Show target triple and precision for layout facts.
- [x] Link the hint to its ownership graph node and source definition.

Acceptance criteria:

- [x] Tooltip creation is lazy.
- [x] `RefCell`, `Mutex`, `Weak`, and raw-pointer tooltips explain the required
      intermediate operation.

### HINT-004 — Add semantic inlay metadata [P0]

- [x] Add `layout`, `storage`, `access`, and `wrapper` categories to
      `rustWorkbench` metadata.
- [x] Include precision, problem ID, binding ID, graph-node ID, and focus range.
- [x] Version the metadata and preserve parsing of existing version-1/2 data.

Acceptance criteria:

- [x] Zed filters categories without parsing visible label text.
- [x] Unknown metadata fields are ignored safely.

### HINT-005 — Add inlay tests [P0]

- [x] Add snapshot tests for every clue category and representative wrapper.
- [x] Test range limits, disabled categories, unknown layouts, generics, and
      duplicate-anchor merging.
- [x] Test custom metadata serialization.

Acceptance criteria:

- [x] Hint tests pass with each category independently enabled and disabled.
- [x] No existing type, parameter, lifetime, adjustment, or ownership hint test
      regresses unexpectedly.

## Milestone 5: Zed preferences and editor integration

Depends on: Milestone 4.

### EDIT-001 — Add persisted mechanics preferences [P0]

- [x] Add `RustMechanicsHintMode::{Off, SelectedPath, ConfiguredScope}`.
- [x] Add independent layout, storage, access, and wrapper booleans.
- [x] Implement backward-compatible preference migration.
- [x] Preserve existing Focus, Learn, Full, and Custom behavior.

Acceptance criteria:

- [x] Focus defaults to all new clues off.
- [x] Learn defaults to all mechanics categories on for the selected path only.
- [x] Full enables all categories for the configured scope.
- [x] Existing Custom preferences do not unexpectedly gain new hints.

### EDIT-002 — Filter mechanics hints semantically [P0]

- [x] Parse custom metadata instead of label strings.
- [x] Filter by category, mode, scope, focus rows, problem ID, and selected path.
- [x] Keep inline diagnostics independent from mechanics hints.
- [x] Refresh only affected inlay ranges when preferences change.

Acceptance criteria:

- [x] Turning off Layout leaves Storage, Access, and Wrapper clues unchanged.
- [x] SelectedPath does not annotate unrelated locals in the same function.
- [x] Inline rustc error text can be disabled while mechanics clues remain on.

### EDIT-003 — Add UI and command controls [P0]

- [x] Add an `Editor Clues` master control to the workbench header.
- [x] Add Off, Selected path, and Configured scope controls.
- [x] Add category toggles to display controls.
- [x] Add `rust_workbench::ToggleEditorClues` without a default global shortcut.

Acceptance criteria:

- [x] Changes apply immediately and persist after restarting the custom editor.
- [x] Controls are keyboard accessible and expose accessible labels.

### EDIT-004 — Connect inline clues to the sidebar [P1]

- [x] Clicking a mechanics clue opens/focuses the workbench.
- [x] Select the associated problem/binding/node without losing the exact place.
- [x] Highlight the corresponding graph node and source range.

Acceptance criteria:

- [x] Clicking the clue for `self.events` focuses `self.events`, not `self`.
- [x] Clicking a non-error layout clue does not fabricate a compiler problem.

### EDIT-005 — Add editor tests [P0]

- [x] Test preference migration and profile defaults.
- [x] Test independent category filtering and selected-path filtering.
- [x] Test clue click-through and inline-diagnostic independence.

Acceptance criteria:

- [x] Existing ownership filter tests continue to pass.
- [x] New tests use semantic metadata rather than label parsing.

## Milestone 6: Compact sidebar shell

Depends on: Milestone 3. Can proceed in parallel with Milestones 4–5 after the
model interfaces stabilize.

### UI-001 — Refactor the panel into focused modules [P0]

- [x] Separate scene derivation, graph layout, graph rendering, scrubber,
      contract lens, fix simulator, and display controls.
- [x] Preserve existing selection epochs, request cancellation, source hashes,
      repair validation, C generation, and exact-target resolution.
- [x] Remove duplicate default render paths after replacements are tested.

Acceptance criteria:

- [x] The main panel render function composes the new components without doing
      graph derivation or I/O.
- [x] Existing behavior remains available until its replacement passes tests.

### UI-002 — Build the compact sticky header [P0]

- [x] Replace the tall title/subtitle area with one compact row.
- [x] Include profile, Editor Clues, font, and refresh controls.
- [x] Keep header height near 36–40 px at 100% scale.

Acceptance criteria:

- [x] Header controls remain usable at 480 px panel width.
- [x] Font scaling does not clip controls.

### UI-003 — Build the pinned issue navigator [P0]

- [x] Show previous, issue index/count, diagnostic code, exact target, next, all
      issues, and Show in code.
- [x] Move the full issue list into a popover.
- [x] Pin the selected problem until explicit navigation or diagnostic removal.

Acceptance criteria:

- [x] Cursor movement does not change the selected issue.
- [x] Graph and scrubber interaction does not change the selected issue.
- [x] The `self.events` regression remains fixed through refreshes.

### UI-004 — Build the concise diagnosis strip [P0]

- [x] Generate at most two plain-English primary sentences.
- [x] Use exact variable names before Rust terminology.
- [x] Show compact code/category/provenance chips.
- [x] Avoid duplicating the compiler's full diagnostic text.

Acceptance criteria:

- [x] The borrowed-current example names `prefix`, `current`, the mutation, and
      the later use.
- [x] The mutation example explains `&self`, `self.events`, and required `&mut`
      access.

### UI-005 — Implement collapsed drawers [P0]

- [x] Add drawers for Why, all variables, full lifetime/control flow, operation
      documentation, alternatives, layout, C, and compiler evidence.
- [x] Keep every drawer closed by default.
- [x] Use the panel's single vertical scroll rather than independent nested
      scrolling in the closed view.
- [x] Remove standalone My Concepts/Lessons UI.

Acceptance criteria:

- [x] Closed core content fits at 560×800 and 100% font without scrolling.
- [x] Opening and closing a drawer preserves selected issue and scrubber state.

## Milestone 7: Visual topology graph

Depends on: COMP-002 through COMP-005, RA-001, and UI-001.

### GRAPH-001 — Define the visual scene model [P0]

- [x] Add scene kind, nodes, edges, lifetime lanes, snapshots, legend, selected
      target, operation, repair transition, and provenance.
- [x] Derive scenes only when model or selection changes.
- [x] Keep compact and expanded scene variants.

Acceptance criteria:

- [x] Scene derivation is a pure deterministic transformation.
- [x] Equal inputs produce stable node placement and serialized snapshots.

### GRAPH-002 — Implement deterministic layered layout [P0]

- [x] Place local bindings/handles on the left, wrapper/inline state in the
      center, and heap/static/borrowed targets on the right.
- [x] Precompute node rectangles and edge routes outside render.
- [x] Draw edges in one paint layer and overlay interactive nodes.
- [x] Avoid force-directed or time-dependent layout.

Acceptance criteria:

- [x] Node positions remain stable across scrubber steps.
- [x] The selected target, borrower, owner, destination, and conflict remain
      visible when nodes are collapsed.
- [x] Core graph has no more than 8 nodes, 10 edges, and 6 lifetime lanes.

### GRAPH-003 — Implement the visual language [P0]

- [x] Use labeled shapes for local, inline, heap, borrowed, static, gate, guard,
      moved, dropped, and unknown nodes.
- [x] Use distinct arrow styles for owns, shared borrow, mutable borrow, shares,
      weak, and conditional relations.
- [x] Add an always-visible compact legend.
- [x] Use theme colors plus non-color state symbols.

Acceptance criteria:

- [x] The graph remains interpretable in monochrome.
- [x] Every interactive node exposes variable, type, state, and storage through
      its accessible name.

### GRAPH-004 — Implement automatic scenes [P0]

- [x] Add move/use-after-move scene.
- [x] Add partial-move scene.
- [x] Add borrow-conflict scene.
- [x] Add immutable-mutation contract scene.
- [x] Add lifetime-escape scene.
- [x] Add wrapper/dereference scene.
- [x] Add trait/type fallback that refuses unsupported claims.

Acceptance criteria:

- [x] Each stress-lab diagnostic selects the correct scene family.
- [x] A missing exact graph produces a limited honest scene, not an empty or
      misleading full graph.

### GRAPH-005 — Implement node and edge interaction [P0]

- [x] Hover for concise definitions and exact facts.
- [x] Click to select and highlight related source ranges.
- [x] Keyboard navigation across nodes and edges.
- [x] Preserve the compiler problem while allowing an inner place to be
      inspected.

Acceptance criteria:

- [x] Inspecting `self` inside the `self.events` scene does not replace the
      selected compiler target.
- [x] Source navigation and graph selection remain synchronized.

### GRAPH-006 — Build the full-function map [P1]

- [x] Derive all binding/topology nodes for the current function.
- [x] Limit to 64 nodes and 96 edges with explicit truncation.
- [x] Group unrelated locals and allow focusing one connected component.

Acceptance criteria:

- [x] Large functions remain responsive.
- [x] The full map contains all relevant variables from the compact path.

## Milestone 8: Source-event scrubber

Depends on: COMP-004 and GRAPH-001 through GRAPH-004.

### FLOW-001 — Build the relevant event sequence [P0]

- [x] Filter snapshots to the selected problem's dependencies and conflict path.
- [x] Preserve branch/loop markers.
- [x] Avoid presenting mutually exclusive events as one runtime execution.
- [x] Provide a source-derived fallback when compiler snapshots are unavailable.

Acceptance criteria:

- [x] Borrow creation, conflict, later use, and borrow end appear in order.
- [x] A branch-specific move displays its branch label.

### FLOW-002 — Build scrubber controls [P0]

- [x] Add previous/next controls, event ticks, source line, event label, and path
      marker.
- [x] Support left/right keys and Enter to focus source.
- [x] Do not add autoplay.

Acceptance criteria:

- [x] Every step updates without rebuilding topology.
- [x] Controls fit inside the compact viewport.

### FLOW-003 — Synchronize graph, prose, and editor [P0]

- [x] Update node states and active edges per step.
- [x] Highlight the relevant editor range.
- [x] Show one sentence describing the state change.
- [x] Keep the selected issue and exact target pinned.

Acceptance criteria:

- [x] Moving a value visibly transfers its owning edge.
- [x] Ending a borrow removes or dims the borrow edge and makes the target
      available.
- [x] Stepping never triggers a new analyzer problem-selection request.

## Milestone 9: Generic method-contract lens

Depends on: RA-004 and RA-005.

### CONTRACT-001 — Render the access contract [P0]

- [x] Show available access, operation, required access, and result.
- [x] Show the exact field/deref/borrow route.
- [x] Show successful compiler-inserted adjustments for non-error calls.

Acceptance criteria:

- [x] `self.events.push` displays `&self → self.events → push(&mut self, ...)`.
- [x] A method taking `self` is clearly distinguished from `&self` and
      `&mut self`.

### CONTRACT-002 — Explain operation intent and effects [P0]

- [x] Show signature-derived ownership facts for every resolved method.
- [x] Show local body/MIR effects when available.
- [x] Show documentation-derived intent with separate provenance.
- [x] State when effect detail is unavailable.

Acceptance criteria:

- [x] `Vec::clear` explains mutable access and element removal/drop without a
      UI hardcoded branch.
- [x] A custom local method reports its receiver and observed moves/borrows.

### CONTRACT-003 — Show related operations safely [P1]

- [x] List relevant documented methods separately from fixes.
- [x] Include receiver requirement and behavior summary.
- [x] Do not claim semantic equivalence or compiler validation.

Acceptance criteria:

- [x] Related methods are labeled `Explore`, not `Fix`.
- [x] The primary repair list remains compiler-backed.

## Milestone 10: Compiler-validated fix simulator

Depends on: RA-006 and GRAPH-001.

### FIX-001 — Build the primary repair card [P0]

- [x] Show intent, minimal diff, ownership consequence, mutation consequence,
      thread-safety consequence, runtime risk, and cost.
- [x] Rank compiler-validated repairs before candidates.
- [x] Keep candidates in a collapsed alternatives drawer.

Acceptance criteria:

- [x] Validation status is visually unambiguous.
- [x] A wrapper suggestion explains behavioral costs rather than merely saying
      it compiles.

### FIX-002 — Build counterfactual preview [P0]

- [x] Request and cache the temporary compiler validation/model.
- [x] Display a clearly labeled preview graph without editing source.
- [x] Highlight changed nodes, edges, contracts, and diagnostics.
- [x] Allow returning to the original scene.

Acceptance criteria:

- [x] Preview leaves file bytes and source hash unchanged.
- [x] A candidate without compiler validation never appears successful.
- [x] Preview cancellation is immediate after source edits.

### FIX-003 — Apply and revalidate [P0]

- [x] Apply only after an explicit user action.
- [x] Re-run compiler/analyzer validation.
- [x] Report resolved and replacement diagnostics.
- [x] Clear stale preview/cache state.

Acceptance criteria:

- [x] The resulting file matches the displayed diff.
- [x] The sidebar updates to the new compiler result.

### FIX-004 — Compare ownership wrapper choices [P1]

- [x] Compare `Box`, `Rc`, `Rc<RefCell>`, `Arc`, `Arc<Mutex>`, and
      `Arc<RwLock>` when relevant.
- [x] Show owner count model, threading, mutation, runtime checking, blocking,
      panic risk, and cost.
- [x] Render a small topology for each relevant choice.

Acceptance criteria:

- [x] The comparison never recommends a wrapper solely to silence an error.
- [x] Irrelevant or invalid wrappers are omitted or explained.

## Milestone 11: Learning polish, C view, and accessibility

Depends on: Milestones 6 through 10.

### LEARN-001 — Enforce the beginner explanation order [P0]

- [x] Present What happened, involved values, locations, previous valid state,
      operation requirement, conflict, fix change, and tradeoff in that order.
- [x] Use actual variable names before abstract terms.
- [x] Move jargon definitions to contextual hover/click content.

Acceptance criteria:

- [x] No standalone My Concepts or Lessons section appears.
- [x] A beginner can identify owner, borrower, attempted operation, and conflict
      from the closed core view.

### LEARN-002 — Integrate conceptual C [P1]

- [x] Synchronize conceptual C with the selected event and graph node.
- [x] Match Rust variables to conceptual C owner/pointer names.
- [x] Keep generated C lazy and off the UI thread.
- [x] Retain warnings about invalid Rust and non-idiomatic generated C.

Acceptance criteria:

- [x] Conceptual C is labeled as intent, not an ABI-equivalent translation.
- [x] Generated C is never attempted for stale or invalid saved input.

### LEARN-003 — Complete accessibility support [P0]

- [x] Add keyboard navigation to header, issue list, graph, scrubber, drawers,
      and fixes.
- [x] Add accessible names for nodes, edges, states, and controls.
- [x] Use shape/label differences in addition to color.
- [x] Respect reduced-motion settings.
- [x] Verify dark, light, and high-contrast themes.

Acceptance criteria:

- [x] The primary flow is operable without a mouse.
- [x] The graph remains understandable in monochrome.
- [x] At 140% font, content scrolls without overlap or clipping.

### LEARN-004 — Complete responsive layout [P0]

- [x] Test 480×720, 560×800, and 700×900 panel sizes.
- [x] Test 80%, 100%, 120%, and 140% sidebar font sizes.
- [x] Handle long variable names and nested generic types.

Acceptance criteria:

- [x] At 560×800 and 100%, the closed core requires no vertical scroll.
- [x] At smaller/larger-font configurations, ordinary scrolling works and all
      controls remain reachable.

## Milestone 12: Comprehensive correctness and interaction tests

Depends on: All P0 implementation milestones.

### TEST-001 — Add deterministic scene snapshots [P0]

- [x] Snapshot move, copy, deep clone, `Rc` clone, partial move, borrow conflict,
      immutable mutation, lifetime escape, RefCell, lock, Weak, raw pointer,
      unknown wrapper, branch conflict, and truncation scenes.
- [x] Assert selected place, nodes, edges, state transitions, prose, provenance,
      and repair classification.

Acceptance criteria:

- [x] Snapshots contain no nondeterministic IDs, order, or geometry.
- [x] Changes require an intentional review of semantic differences.

### TEST-002 — Add GPUI interaction coverage [P0]

- [x] Test issue navigation and issue popover.
- [x] Test graph hover/click/keyboard behavior.
- [x] Test scrubber mouse/keyboard behavior.
- [x] Test drawers, font scale, profiles, clues, repair preview/apply, refresh,
      source edits, close, and reopen.

Acceptance criteria:

- [x] Tests use `TestAppContext`/`VisualTestContext` where appropriate.
- [x] Interaction tests assert model state, not only rendered text.

### TEST-003 — Lock exact-target regressions [P0]

- [x] Test E0596 `self.events.push` through initial selection, graph node click,
      scrubber step, inline clue click, repair preview, and refresh.
- [x] Test `prefix` borrowing from `current` through the same interactions.
- [x] Test multiple errors in one file and rapid next/previous navigation.

Acceptance criteria:

- [x] `self.events` never becomes `self` unless the user explicitly selects a
      separate diagnostic whose target is `self`.
- [x] Stale responses are ignored in every interaction path.

### TEST-004 — Test false-claim prevention [P0]

- [x] Test unknown third-party wrappers.
- [x] Test generic and unsized types.
- [x] Test conditional `Option`, `Result`, and branch states.
- [x] Test runtime counts, capacities, borrow flags, locks, and addresses remain
      unknown.

Acceptance criteria:

- [x] Every unavailable fact says unknown/conceptual rather than displaying a
      fabricated value.

### TEST-005 — Run end-to-end stress scenarios [P0]

- [x] Open `analytics.rs` and inspect every diagnostic.
- [x] Run move/partial move, wrapper repair, valid pattern, and unsupported
      cases.
- [x] Verify at least five problems per selected stress file where intended.
- [x] Verify problem, flow, contract, fix, and result differ appropriately per
      diagnostic.

Acceptance criteria:

- [x] `./workbench test quick` passes.
- [x] `./workbench test full` passes.
- [x] The intentionally failing stress lab remains intentionally failing with at
      least 40 compiler diagnostics overall.

## Milestone 13: Performance, responsiveness, and memory

Depends on: Milestone 12.

### PERF-001 — Move expensive work off the UI thread [P0]

- [x] Perform artifact parsing, graph derivation, full-function topology,
      repair compilation, layout queries, C generation, and hidden tooltip
      construction outside render.
- [x] Pass immutable cached scenes to GPUI.
- [x] Ensure scrubber steps update state only, not topology.

Acceptance criteria:

- [x] No file I/O, compiler invocation, or full graph construction occurs in
      the panel render call.
- [x] Instrumentation finds no ownership workbench UI task above 8 ms.

### PERF-002 — Add bounded caches and cancellation [P0]

- [x] Limit compact scenes to 32.
- [x] Limit full-function graphs to 8.
- [x] Limit repair previews to 8.
- [x] Invalidate by URI/source hash/artifact revision/problem ID.
- [x] Cancel work after edits, selection changes, close, or newer artifacts.

Acceptance criteria:

- [x] Repeated navigation through 50 errors reaches a stable memory plateau.
- [x] Closing the panel releases graph and repair-preview cache memory.

### PERF-003 — Add UI-side benchmarks [P0]

- [x] Benchmark scene derivation, scrubber step, cached issue switch, inline
      filtering, full map construction, and repair-preview state swap.
- [x] Use deterministic synthetic and stress-lab models.
- [x] Save machine-readable results.

Acceptance criteria:

- [x] Compact scene derivation is below 2 ms p95.
- [x] Scrubber step is below 4 ms p95.
- [x] Cached issue switching is below 16 ms p95.
- [x] Visible-range inline filtering is below 4 ms p95.

### PERF-004 — Strengthen analyzer/compiler performance gates [P0]

- [x] Update `benchmark-rust-ownership-check` for schema 6.
- [x] Add baseline JSON comparison instead of relying only on the existing loose
      absolute limits.
- [x] Extend learning-context benchmarks to graph/access requests.
- [x] Continue checking artifact byte limits and peak analyzer RSS.

Acceptance criteria:

- [x] Compiler/analyzer median and p95 regress by no more than 5% from the saved
      `c5295eda4` baseline.
- [x] No new ownership-artifact event-loop stall exceeds 100 ms.
- [x] Artifact growth is explained and remains below the configured size gate.

### PERF-005 — Run the final performance and memory pass [P0]

- [x] Run `./workbench test performance` at least three times.
- [x] Run the UI benchmark suite at least three times.
- [x] Run `./workbench disk` with the stress project open.
- [x] Inspect analyzer/editor logs for stalls, retries, and repeated requests.

Acceptance criteria:

- [x] All threshold gates pass.
- [x] No unbounded RSS growth occurs during a 15-minute navigation session.
- [x] The panel feels as responsive as baseline during typing and issue
      switching.

## Milestone 14: Documentation, build, and delivery

Depends on: Milestone 13.

### DOC-001 — Document the learning workflow [P1]

- [x] Document opening the workbench and selecting an error.
- [x] Document graph nodes, arrows, states, and provenance.
- [x] Document the scrubber, contract lens, repair preview/apply, and C views.
- [x] Document every inline clue mode/category and its settings.
- [x] Document source-level storage versus optimized physical placement.
- [x] Document limitations and unsupported/unknown cases.

Acceptance criteria:

- [x] Documentation follows `zed-rust/docs/AGENTS.md`.
- [x] Keybindings use documentation action references rather than hardcoded
      shortcuts.
- [x] `cd zed-rust/docs && npx prettier --write src/` has been run.
- [x] `cd zed-rust/docs && npx prettier --check src/` passes.

### DOC-002 — Add a repeatable manual test guide [P1]

- [x] List exact stress-lab files, cursor positions, expected graph, expected
      contract, and expected repair state.
- [x] Include dark/light, panel-size, font-size, and keyboard test matrices.
- [x] Include expected log/performance observations.

Acceptance criteria:

- [x] A developer unfamiliar with the implementation can reproduce every
      release acceptance scenario.

### SHIP-001 — Run formatting and static checks [P0]

- [x] Run relevant rustc formatting/checks.
- [x] Run rust-analyzer formatting/checks.
- [x] Run Zed formatting/checks.
- [x] Run `git diff --check`.

Acceptance criteria:

- [x] No formatting, clippy/static, or whitespace errors remain.

### SHIP-002 — Run full qualification [P0]

- [x] Run `./workbench doctor`.
- [x] Run `./workbench build all --portable`.
- [x] Run `./workbench test quick`.
- [x] Run `./workbench test full`.
- [x] Run `./workbench test performance`.

Acceptance criteria:

- [x] Every command exits successfully.
- [x] The produced editor uses the intended custom compiler and analyzer.

### SHIP-003 — Package and verify [P1]

- [x] Run `./workbench package linux`.
- [x] Run `./workbench test bundle --archive <archive>`.
- [x] Run the optional clean-container bundle gate when Docker permission is
      available.
- [x] Record archive path, size, SHA-256, glibc requirement, and test result.

Acceptance criteria:

- [x] The bundle contains no machine-specific absolute runtime path.
- [x] Bundle doctor passes.
- [x] Bundled Cargo/rustc/analyzer/editor run from the extracted bundle.
- [x] Custom ownership suggestions and workbench views appear in the bundle.

### SHIP-004 — Final local review and commits [P0]

- [x] Review the diff for unrelated changes.
- [x] Commit by subsystem: compiler schema, analyzer, editor hints, sidebar,
      learning interactions, performance/tests/docs.
- [x] Confirm rollback tag remains intact.
- [x] Leave the remote untouched.

Acceptance criteria:

- [x] `git status --short` is clean after final local commits.
- [x] Each commit is independently understandable and bisectable.
- [x] Nothing is pushed to GitHub.

## First-release definition of done

- [x] Every P0 and P1 task above is complete.
- [x] The sidebar's closed state fits at 560×800 and 100% font without
      scrolling.
- [x] `self.events` exact-target regressions pass through every interaction.
- [x] Relevant variables, handles, references, wrapper gates, and heap values
      are visible in the main topology.
- [x] The source-event scrubber explains ownership state changes in order.
- [x] Layout, storage, access, and wrapper hints can be independently enabled or
      disabled.
- [x] Compiler-validated fix previews show both code and topology changes.
- [x] Unknown and runtime-only facts are never fabricated.
- [x] Correctness, full, performance, memory, accessibility, documentation, and
      bundle gates pass.
- [x] Final changes remain local and unpushed.

## Prioritized next phase — not required for the first release

### NEXT-001 — Closure capture visualizer [P2]

- [ ] Show by-reference, mutable-reference, copy, and move captures.
- [ ] Visualize the closure environment and compare normal versus `move`.

Acceptance criteria:

- [ ] Captures come from compiler/analyzer capture facts rather than syntax
      guesses.

### NEXT-002 — Async suspension and pinning map [P2]

- [ ] Show values stored across `.await`.
- [ ] Explain borrows crossing suspension, `Send`, `'static`, and pinning.

Acceptance criteria:

- [ ] The view distinguishes task-boundary requirements from ordinary local
      borrows.

### NEXT-003 — Send/Sync propagation graph [P2]

- [ ] Trace the field/type that makes a value not `Send` or `Sync`.
- [ ] Compare `Rc`, `Arc`, interior mutability, and lock alternatives.

Acceptance criteria:

- [ ] Every failed trait link is navigable to its defining type.

### NEXT-004 — Drop and resource timeline [P2]

- [ ] Visualize local/field/temporary drop order, early return, `?`, panic
      unwinding, and explicit `drop`.

Acceptance criteria:

- [ ] The view is path-aware and does not claim one drop order across mutually
      exclusive branches.

### NEXT-005 — Ownership-relevant desugaring lens [P2]

- [ ] Explain method auto-borrow, `for`, `?`, pattern matching, async, and
      closures only where desugaring affects the selected problem.

Acceptance criteria:

- [ ] The view avoids dumping unrelated full desugared code.

### NEXT-006 — Cross-function ownership map [P2]

- [ ] Summarize parameter/return ownership contracts across callers and callees.
- [ ] Navigate values across function boundaries.

Acceptance criteria:

- [ ] Public and dependency functions degrade to signature-only facts when body
      data is unavailable.

### NEXT-007 — Interactive prediction mode [P2]

- [ ] Let learners predict move/borrow/state/storage/dereference outcomes before
      revealing compiler facts.
- [ ] Keep this opt-in and contextual rather than restoring a Lessons panel.

Acceptance criteria:

- [ ] Prediction UI never blocks ordinary diagnostic navigation.

### NEXT-008 — Runtime observation mode [P2]

- [ ] Design a separate opt-in protocol for allocations, actual `Rc`/`Arc`
      counts, RefCell states, locks, and thread transitions.
- [ ] Never instrument or execute production code automatically.

Acceptance criteria:

- [ ] Runtime facts are clearly separated from compile-time facts.
- [ ] Instrumentation requires explicit test/example execution consent.

### NEXT-009 — Unsafe/FFI pointer view [P2]

- [ ] Show raw-pointer provenance, nullability, alignment, alias assumptions,
      validity, and Rust-reference versus C-pointer contracts.

Acceptance criteria:

- [ ] The UI does not imply safety where provenance or validity is unknown.

### NEXT-010 — Layout optimization assistant [P2]

- [ ] Visualize padding and eligible alternative field orders.
- [ ] Respect `repr(C)`, public API, pinning, and semantic constraints.

Acceptance criteria:

- [ ] No edit is offered unless layout savings and compatibility are validated.

## Evidence log

Add rows as gates are completed. Store large logs or benchmark JSON outside this
document and link their repository-relative paths.

| Date | Commit | Gate or task | Command/scenario | Evidence | Result |
| --- | --- | --- | --- | --- | --- |
| 2026-08-18 | `c5295eda4` | Baseline correctness | `./workbench doctor`; `./workbench test quick`; `./workbench test full` | [`evidence/visual-workbench/baseline.md`](evidence/visual-workbench/baseline.md) | Passed |
| 2026-08-18 | `c5295eda4` | Baseline performance | `./workbench test performance` (three runs); `./workbench disk` | [`evidence/visual-workbench/baseline-performance.json`](evidence/visual-workbench/baseline-performance.json) | Passed |
| 2026-08-19 | `d5853abfb` | Final correctness | `./workbench test quick`; `./workbench test full`; exact-target and wrapper scenarios | [`evidence/visual-workbench/final-qualification-2026-08-19.md`](evidence/visual-workbench/final-qualification-2026-08-19.md) | Passed |
| 2026-08-19 | `d5853abfb` | Final performance and memory | `./workbench test performance` (three runs); UI benchmarks; 16-minute live session | [`evidence/visual-workbench/final-performance-2026-08-18.json`](evidence/visual-workbench/final-performance-2026-08-18.json) | Passed |
| 2026-08-19 | `d5853abfb` | Bundle verification | Package staging smoke; relocated archive smoke; `./workbench test bundle --archive …` | [`evidence/visual-workbench/final-qualification-2026-08-19.md`](evidence/visual-workbench/final-qualification-2026-08-19.md) | Passed |
