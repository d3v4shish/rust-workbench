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

- [ ] Every P0/P1 implementation task in that milestone is checked.
- [ ] Every acceptance criterion is checked.
- [ ] The listed automated tests pass.
- [ ] Required manual checks have recorded evidence.
- [ ] No unrelated user changes were modified or removed.

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
- [ ] Run `./workbench disk` with the editor and analyzer open on the stress lab.
- [ ] Record editor RSS, analyzer RSS, artifact bytes, and event-loop warnings.
- [ ] Record sidebar issue-switch latency if an existing harness supports it.

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

- [ ] Bump `BORROWCK_OWNERSHIP_MODEL_SCHEMA_VERSION` from 5 to 6.
- [ ] Add serialized memory graph, node, edge, snapshot, access-path, and access
      step types.
- [ ] Add stable serialized enums for storage region, node kind, edge relation,
      access kind, ownership state, and provenance.
- [ ] Add `#[serde(default)]` or optional fields wherever older consumers must
      remain compatible.
- [ ] Document source-level storage semantics separately from optimized physical
      placement.

Acceptance criteria:

- [ ] A schema-6 artifact round-trips through serialization tests.
- [ ] The artifact contains no duplicated source file text.
- [ ] Unknown enum data fails safely or maps to an explicit `unknown` value.
- [ ] Artifact consumers can distinguish exact, derived, conceptual, and unknown
      facts.

### COMP-002 — Emit stable memory nodes [P0]

- [ ] Emit one stable node for each relevant local binding and exact MIR place.
- [ ] Emit separate nodes for handles, inline wrapper state, heap allocations,
      buffers, control blocks, guards, and borrowed views.
- [ ] Attach type, size, alignment, source range, body ID, place, state, and
      provenance.
- [ ] Use stable IDs derived from body/place/layer identity rather than vector
      position.
- [ ] Cap recursive topology depth at 12 and mark truncation explicitly.

Acceptance criteria:

- [ ] Reordering unrelated bindings does not change IDs for unchanged places.
- [ ] A local `Vec<T>` has a distinct local header and runtime-sized buffer node.
- [ ] A `Box<T>` has a local handle and a heap allocation for `T`.
- [ ] Generic or unsized layouts are represented without fabricating a size.

### COMP-003 — Emit ownership and pointer edges [P0]

- [ ] Emit `owns`, `contains`, `owns_buffer`, `points_to`, `borrow_shared`,
      `borrow_mutable`, `reborrow`, `shares_allocation`, `weak_reference`,
      `guards_access`, `conditional`, and `moved_to` relations where supported.
- [ ] Attach the related event, loan, source range, and provenance.
- [ ] Preserve projections such as fields, indexes, slices, and dereferences.
- [ ] Represent reference-to-field relationships without collapsing them to the
      root local.

Acceptance criteria:

- [ ] `prefix: &str` points to the relevant place inside `current`.
- [ ] A move transfers an ownership relation to its destination.
- [ ] `Rc::clone` and `Arc::clone` produce multiple handles sharing one
      allocation rather than multiple allocations.
- [ ] A weak pointer targets the control block with a weak edge.

### COMP-004 — Emit ownership-relevant snapshots [P0]

- [ ] Emit snapshots for initialization, borrow reservation, activation, move,
      copy, clone, reborrow, last use, borrow end, conflict, reinitialization,
      and drop.
- [ ] Record source ranges and MIR locations.
- [ ] Preserve branch/loop path markers so mutually exclusive paths are not
      displayed as a single execution.
- [ ] Emit state deltas instead of duplicating the entire graph where practical.

Acceptance criteria:

- [ ] The borrowed-current fixture shows borrow creation, rejected mutation,
      later reference use, and borrow end in the correct order.
- [ ] A two-phase mutable borrow distinguishes reservation and activation.
- [ ] A conditional move is labeled with its control-flow path.
- [ ] Snapshot size remains bounded on the stress lab.

### COMP-005 — Emit typed access paths [P0]

- [ ] Record built-in dereference, trait `Deref`, trait `DerefMut`, auto-borrow,
      explicit raw-pointer dereference, wrapper access, and guard dereference.
- [ ] Record starting type, result type, mutability, explicitness, fallibility,
      panic risk, and unsafe requirement.
- [ ] Represent `Weak::upgrade`, `Option`/`Result` extraction, and lock/borrow
      guard creation.

Acceptance criteria:

- [ ] `Box<T>` reports built-in dereference to `T`.
- [ ] `Rc<T>` reports shared dereference and does not claim general mutable
      dereference.
- [ ] `RefCell<T>` reports that `borrow`/`borrow_mut` is required.
- [ ] A raw pointer reports explicit unsafe dereference.

### COMP-006 — Add compiler schema tests [P0]

- [ ] Add serialization tests for every new type and enum.
- [ ] Add compiler tests for moves, copies, clones, partial moves, borrows,
      reborrows, and reinitialization.
- [ ] Add nested-wrapper tests for `Rc<RefCell<Vec<T>>>` and
      `Arc<Mutex<Vec<T>>>`.
- [ ] Add branch, loop, early-return, fat-pointer, raw-pointer, generic, unsized,
      zero-sized, and truncation tests.
- [ ] Update artifact-size/performance scripts that currently require schema 5.

Acceptance criteria:

- [ ] `cd rust && ./x test --force-rerun tests/run-make/borrowck-autofix`
      passes.
- [ ] All new schema fixtures validate as version 6.
- [ ] Existing schema-5 fixtures remain readable by downstream compatibility
      tests.

## Milestone 2: Compiler topology coverage

Depends on: COMP-001 through COMP-006.

### TOPO-001 — Model references and direct pointers [P0]

- [ ] Model `&T`, `&mut T`, `&str`, slices, trait-object fat pointers, raw
      pointers, and `NonNull<T>`.
- [ ] Separate the pointer/reference representation from its pointee.
- [ ] Show metadata words for fat pointers conceptually without inventing
      runtime values.
- [ ] Preserve exact field and element projection targets.

Acceptance criteria:

- [ ] Shared and mutable references use distinct edges and states.
- [ ] Slice and trait-object references are identified as fat pointers.
- [ ] Raw pointers are not presented as owners or safe borrows.

### TOPO-002 — Model owning and shared smart pointers [P0]

- [ ] Model `Box`, `Rc`, `rc::Weak`, `Arc`, `sync::Weak`, and `Pin<P>`.
- [ ] Separate the `Rc`/`Arc` handle, control block, and inner value.
- [ ] Represent pinning as a movement constraint, not a storage location.
- [ ] Treat source-visible handles separately from runtime reference counts.

Acceptance criteria:

- [ ] `let b = Rc::clone(&a)` produces two handles and one allocation.
- [ ] Moving an `Rc` handle invalidates the source binding without duplicating
      the allocation.
- [ ] `Pin<Box<T>>` shows both ownership and the pinning constraint.

### TOPO-003 — Model interior mutability and synchronization [P0]

- [ ] Model `Cell`, `RefCell`, `UnsafeCell`, `Mutex`, `RwLock`, `OnceCell`, and
      `OnceLock`.
- [ ] Add visual gate nodes for runtime borrow or synchronization checks.
- [ ] Model returned guards as temporary values that provide dereference access.
- [ ] Distinguish panic, fallible, blocking, and unsafe access behavior.

Acceptance criteria:

- [ ] `Rc<RefCell<Vec<T>>>` renders recursively through every wrapper.
- [ ] `Arc<Mutex<T>>` shows shared ownership followed by an exclusive lock gate.
- [ ] No runtime lock or borrow state is claimed without runtime evidence.

### TOPO-004 — Model standard allocating containers [P1]

- [ ] Model `Vec`, `String`, `VecDeque`, `BinaryHeap`, `PathBuf`, and `OsString`.
- [ ] Model `HashMap`, `HashSet`, `BTreeMap`, `BTreeSet`, and `LinkedList` using
      standard-library conceptual allocation contracts.
- [ ] Model boxed slices and boxed strings.
- [ ] Mark runtime length, capacity, buckets, and node counts as unknown.

Acceptance criteria:

- [ ] Every container separates its fixed-size local representation from
      runtime allocation where applicable.
- [ ] Conceptual collection internals are labeled as standard-library semantics,
      not exact physical layout.
- [ ] No std-version-specific private field layout is exposed as a stable fact.

### TOPO-005 — Model aggregates and conditional ownership [P1]

- [ ] Model arrays, tuples, structs, unions, enums, `Option`, `Result`, and `Cow`.
- [ ] Show exact inline layout and field containment where available.
- [ ] Show borrowed/owned alternatives for `Cow` as conditional branches.
- [ ] Treat custom wrappers as opaque unless ownership can be proven.

Acceptance criteria:

- [ ] `Option<Box<T>>` shows conditional ownership without asserting the active
      variant.
- [ ] A custom `MyBox<T>` does not become an owning pointer based on its name.
- [ ] Struct padding and field offsets match target layout data.

## Milestone 3: Rust-analyzer ingestion and fallback model

Depends on: Milestones 1 and 2.

### RA-001 — Ingest schema 6 and preserve older schemas [P0]

- [ ] Accept compiler artifact schemas 2 through 6.
- [ ] Convert graph nodes, edges, snapshots, and access paths into analyzer model
      types.
- [ ] Keep all new LSP fields optional and bump the extended response schema to
      12.
- [ ] Preserve URI, source hash, artifact revision, selected problem ID, and
      exact place through every conversion.

Acceptance criteria:

- [ ] Schema-6 models expose the full graph.
- [ ] Schema 2–5 artifacts still return the best model available from their
      fields.
- [ ] Unsupported future schemas fail with a clear status instead of crashing.

### RA-002 — Merge exact and estimated facts [P0]

- [ ] Build a conservative analyzer-only scene before Cargo check completes.
- [ ] Use type inference, method resolution, expression adjustments, reference
      search, and control-flow data.
- [ ] Replace estimated facts with compiler-exact facts after a valid artifact
      arrives.
- [ ] Never merge facts from different source hashes or problem IDs.

Acceptance criteria:

- [ ] Estimated facts are visibly marked `analyzer estimate`.
- [ ] Saving/checking upgrades the same selected problem without changing its
      exact target.
- [ ] Stale responses cannot replace a newer scene.

### RA-003 — Produce layout and storage facts [P0]

- [ ] Reuse rust-analyzer's target layout engine for locals, ADTs, fields,
      aliases, arrays, and tuples.
- [ ] Report size, alignment, padding summary, field offset, zero-sized status,
      unsized status, and `Drop` requirement.
- [ ] Separate handle size from runtime allocation size.
- [ ] Attach target triple and provenance.

Acceptance criteria:

- [ ] Struct layout matches existing memory-layout hover results.
- [ ] Generic layouts say that size depends on type parameters.
- [ ] `Vec<T>` never labels 24 bytes as the size of its heap buffer.

### RA-004 — Produce dereference and usability facts [P0]

- [ ] Reuse expression-adjustment and method-resolution data for actual calls.
- [ ] Produce capability facts for selected bindings even before a call exists.
- [ ] Explain automatic versus explicit access and the resulting type.
- [ ] Report fallibility, panic risk, poisoning, and unsafe requirements.

Acceptance criteria:

- [ ] The model accurately distinguishes `Box`, `Rc`, `RefCell`, `Mutex`,
      `Weak`, `Option<Box<T>>`, and raw pointers.
- [ ] Unresolved trait obligations return `unknown` rather than a false access
      path.

### RA-005 — Generalize method contracts [P0]

- [ ] Return the resolved signature and `self`/`&self`/`&mut self` requirement
      for every resolved method.
- [ ] Return argument ownership and return-borrow relationships.
- [ ] Use local MIR/body facts when available.
- [ ] Use documentation summaries separately from proven ownership effects.
- [ ] Remove UI dependence on method-name-specific branches.

Acceptance criteria:

- [ ] `Vec::clear`, `push`, and a user-defined mutable method use the same
      contract pipeline.
- [ ] External methods without bodies still show signature-derived facts.
- [ ] Related methods are labeled as discovery, not validated repairs.

### RA-006 — Extend repair validation with preview models [P0]

- [ ] Allow a validated temporary repair to return an optional ownership model
      or graph delta for the patched source.
- [ ] Key preview results by source hash and repair ID.
- [ ] Cancel validation when source or selection changes.
- [ ] Keep unvalidated candidates separate from compiler-validated previews.

Acceptance criteria:

- [ ] Previewing does not modify the real source file.
- [ ] A validated preview reports whether the selected diagnostic disappears.
- [ ] Replacement diagnostics are reported rather than hidden.

### RA-007 — Add analyzer regression tests [P0]

- [ ] Add tests for schema compatibility, exact-place retention, graph
      conversion, snapshots, layouts, access paths, method contracts, and repair
      previews.
- [ ] Extend the learning-context benchmark assertions for graph nodes and
      access steps.

Acceptance criteria:

- [ ] `cargo test --manifest-path rust/src/tools/rust-analyzer/Cargo.toml -p rust-analyzer ownership_ --lib --bins`
      passes.
- [ ] `cargo test --manifest-path rust/src/tools/rust-analyzer/Cargo.toml -p rust-analyzer learning_problem --lib`
      passes.
- [ ] `cargo test --manifest-path rust/src/tools/rust-analyzer/Cargo.toml -p ide ownership_insight --lib`
      passes.

## Milestone 4: Inline type-mechanics clues

Depends on: RA-003 through RA-005.

### HINT-001 — Add mechanics hint configuration [P0]

- [ ] Add rust-analyzer configuration for mechanics master enable, layout,
      storage, access, and wrapper hints.
- [ ] Keep the feature disabled by default in stock rust-analyzer.
- [ ] Enable it through the custom Zed language configuration when the
      workbench requires it.
- [ ] Trigger inlay refresh when settings change.

Acceptance criteria:

- [ ] VS Code or another standard LSP client can enable each category through
      rust-analyzer settings.
- [ ] A client that does not understand custom metadata still renders a normal
      inlay label.

### HINT-002 — Generate compact hint labels [P0]

- [ ] Generate layout labels such as `16 B · align 8 · inline`.
- [ ] Generate storage labels such as `24 B handle → heap buffer`.
- [ ] Generate access labels such as `auto-deref Rc → T · shared`.
- [ ] Generate wrapper labels such as `borrow_mut() → RefMut<T>`.
- [ ] Merge categories at the same source anchor into one label.

Acceptance criteria:

- [ ] At most one mechanics label appears at a given anchor.
- [ ] Compact labels do not include unsupported runtime values.
- [ ] Long types are shortened in the label and complete in the tooltip.

### HINT-003 — Add detailed lazy tooltips [P0]

- [ ] Show the complete type/access chain.
- [ ] Explain whether access is automatic, explicit, fallible, panicking,
      blocking, or unsafe.
- [ ] Show target triple and precision for layout facts.
- [ ] Link the hint to its ownership graph node and source definition.

Acceptance criteria:

- [ ] Tooltip creation is lazy.
- [ ] `RefCell`, `Mutex`, `Weak`, and raw-pointer tooltips explain the required
      intermediate operation.

### HINT-004 — Add semantic inlay metadata [P0]

- [ ] Add `layout`, `storage`, `access`, and `wrapper` categories to
      `rustWorkbench` metadata.
- [ ] Include precision, problem ID, binding ID, graph-node ID, and focus range.
- [ ] Version the metadata and preserve parsing of existing version-1/2 data.

Acceptance criteria:

- [ ] Zed filters categories without parsing visible label text.
- [ ] Unknown metadata fields are ignored safely.

### HINT-005 — Add inlay tests [P0]

- [ ] Add snapshot tests for every clue category and representative wrapper.
- [ ] Test range limits, disabled categories, unknown layouts, generics, and
      duplicate-anchor merging.
- [ ] Test custom metadata serialization.

Acceptance criteria:

- [ ] Hint tests pass with each category independently enabled and disabled.
- [ ] No existing type, parameter, lifetime, adjustment, or ownership hint test
      regresses unexpectedly.

## Milestone 5: Zed preferences and editor integration

Depends on: Milestone 4.

### EDIT-001 — Add persisted mechanics preferences [P0]

- [ ] Add `RustMechanicsHintMode::{Off, SelectedPath, ConfiguredScope}`.
- [ ] Add independent layout, storage, access, and wrapper booleans.
- [ ] Implement backward-compatible preference migration.
- [ ] Preserve existing Focus, Learn, Full, and Custom behavior.

Acceptance criteria:

- [ ] Focus defaults to all new clues off.
- [ ] Learn defaults to all mechanics categories on for the selected path only.
- [ ] Full enables all categories for the configured scope.
- [ ] Existing Custom preferences do not unexpectedly gain new hints.

### EDIT-002 — Filter mechanics hints semantically [P0]

- [ ] Parse custom metadata instead of label strings.
- [ ] Filter by category, mode, scope, focus rows, problem ID, and selected path.
- [ ] Keep inline diagnostics independent from mechanics hints.
- [ ] Refresh only affected inlay ranges when preferences change.

Acceptance criteria:

- [ ] Turning off Layout leaves Storage, Access, and Wrapper clues unchanged.
- [ ] SelectedPath does not annotate unrelated locals in the same function.
- [ ] Inline rustc error text can be disabled while mechanics clues remain on.

### EDIT-003 — Add UI and command controls [P0]

- [ ] Add an `Editor Clues` master control to the workbench header.
- [ ] Add Off, Selected path, and Configured scope controls.
- [ ] Add category toggles to display controls.
- [ ] Add `rust_workbench::ToggleEditorClues` without a default global shortcut.

Acceptance criteria:

- [ ] Changes apply immediately and persist after restarting the custom editor.
- [ ] Controls are keyboard accessible and expose accessible labels.

### EDIT-004 — Connect inline clues to the sidebar [P1]

- [ ] Clicking a mechanics clue opens/focuses the workbench.
- [ ] Select the associated problem/binding/node without losing the exact place.
- [ ] Highlight the corresponding graph node and source range.

Acceptance criteria:

- [ ] Clicking the clue for `self.events` focuses `self.events`, not `self`.
- [ ] Clicking a non-error layout clue does not fabricate a compiler problem.

### EDIT-005 — Add editor tests [P0]

- [ ] Test preference migration and profile defaults.
- [ ] Test independent category filtering and selected-path filtering.
- [ ] Test clue click-through and inline-diagnostic independence.

Acceptance criteria:

- [ ] Existing ownership filter tests continue to pass.
- [ ] New tests use semantic metadata rather than label parsing.

## Milestone 6: Compact sidebar shell

Depends on: Milestone 3. Can proceed in parallel with Milestones 4–5 after the
model interfaces stabilize.

### UI-001 — Refactor the panel into focused modules [P0]

- [ ] Separate scene derivation, graph layout, graph rendering, scrubber,
      contract lens, fix simulator, and display controls.
- [ ] Preserve existing selection epochs, request cancellation, source hashes,
      repair validation, C generation, and exact-target resolution.
- [ ] Remove duplicate default render paths after replacements are tested.

Acceptance criteria:

- [ ] The main panel render function composes the new components without doing
      graph derivation or I/O.
- [ ] Existing behavior remains available until its replacement passes tests.

### UI-002 — Build the compact sticky header [P0]

- [ ] Replace the tall title/subtitle area with one compact row.
- [ ] Include profile, Editor Clues, font, and refresh controls.
- [ ] Keep header height near 36–40 px at 100% scale.

Acceptance criteria:

- [ ] Header controls remain usable at 480 px panel width.
- [ ] Font scaling does not clip controls.

### UI-003 — Build the pinned issue navigator [P0]

- [ ] Show previous, issue index/count, diagnostic code, exact target, next, all
      issues, and Show in code.
- [ ] Move the full issue list into a popover.
- [ ] Pin the selected problem until explicit navigation or diagnostic removal.

Acceptance criteria:

- [ ] Cursor movement does not change the selected issue.
- [ ] Graph and scrubber interaction does not change the selected issue.
- [ ] The `self.events` regression remains fixed through refreshes.

### UI-004 — Build the concise diagnosis strip [P0]

- [ ] Generate at most two plain-English primary sentences.
- [ ] Use exact variable names before Rust terminology.
- [ ] Show compact code/category/provenance chips.
- [ ] Avoid duplicating the compiler's full diagnostic text.

Acceptance criteria:

- [ ] The borrowed-current example names `prefix`, `current`, the mutation, and
      the later use.
- [ ] The mutation example explains `&self`, `self.events`, and required `&mut`
      access.

### UI-005 — Implement collapsed drawers [P0]

- [ ] Add drawers for Why, all variables, full lifetime/control flow, operation
      documentation, alternatives, layout, C, and compiler evidence.
- [ ] Keep every drawer closed by default.
- [ ] Use the panel's single vertical scroll rather than independent nested
      scrolling in the closed view.
- [ ] Remove standalone My Concepts/Lessons UI.

Acceptance criteria:

- [ ] Closed core content fits at 560×800 and 100% font without scrolling.
- [ ] Opening and closing a drawer preserves selected issue and scrubber state.

## Milestone 7: Visual topology graph

Depends on: COMP-002 through COMP-005, RA-001, and UI-001.

### GRAPH-001 — Define the visual scene model [P0]

- [ ] Add scene kind, nodes, edges, lifetime lanes, snapshots, legend, selected
      target, operation, repair transition, and provenance.
- [ ] Derive scenes only when model or selection changes.
- [ ] Keep compact and expanded scene variants.

Acceptance criteria:

- [ ] Scene derivation is a pure deterministic transformation.
- [ ] Equal inputs produce stable node placement and serialized snapshots.

### GRAPH-002 — Implement deterministic layered layout [P0]

- [ ] Place local bindings/handles on the left, wrapper/inline state in the
      center, and heap/static/borrowed targets on the right.
- [ ] Precompute node rectangles and edge routes outside render.
- [ ] Draw edges in one paint layer and overlay interactive nodes.
- [ ] Avoid force-directed or time-dependent layout.

Acceptance criteria:

- [ ] Node positions remain stable across scrubber steps.
- [ ] The selected target, borrower, owner, destination, and conflict remain
      visible when nodes are collapsed.
- [ ] Core graph has no more than 8 nodes, 10 edges, and 6 lifetime lanes.

### GRAPH-003 — Implement the visual language [P0]

- [ ] Use labeled shapes for local, inline, heap, borrowed, static, gate, guard,
      moved, dropped, and unknown nodes.
- [ ] Use distinct arrow styles for owns, shared borrow, mutable borrow, shares,
      weak, and conditional relations.
- [ ] Add an always-visible compact legend.
- [ ] Use theme colors plus non-color state symbols.

Acceptance criteria:

- [ ] The graph remains interpretable in monochrome.
- [ ] Every interactive node exposes variable, type, state, and storage through
      its accessible name.

### GRAPH-004 — Implement automatic scenes [P0]

- [ ] Add move/use-after-move scene.
- [ ] Add partial-move scene.
- [ ] Add borrow-conflict scene.
- [ ] Add immutable-mutation contract scene.
- [ ] Add lifetime-escape scene.
- [ ] Add wrapper/dereference scene.
- [ ] Add trait/type fallback that refuses unsupported claims.

Acceptance criteria:

- [ ] Each stress-lab diagnostic selects the correct scene family.
- [ ] A missing exact graph produces a limited honest scene, not an empty or
      misleading full graph.

### GRAPH-005 — Implement node and edge interaction [P0]

- [ ] Hover for concise definitions and exact facts.
- [ ] Click to select and highlight related source ranges.
- [ ] Keyboard navigation across nodes and edges.
- [ ] Preserve the compiler problem while allowing an inner place to be
      inspected.

Acceptance criteria:

- [ ] Inspecting `self` inside the `self.events` scene does not replace the
      selected compiler target.
- [ ] Source navigation and graph selection remain synchronized.

### GRAPH-006 — Build the full-function map [P1]

- [ ] Derive all binding/topology nodes for the current function.
- [ ] Limit to 64 nodes and 96 edges with explicit truncation.
- [ ] Group unrelated locals and allow focusing one connected component.

Acceptance criteria:

- [ ] Large functions remain responsive.
- [ ] The full map contains all relevant variables from the compact path.

## Milestone 8: Source-event scrubber

Depends on: COMP-004 and GRAPH-001 through GRAPH-004.

### FLOW-001 — Build the relevant event sequence [P0]

- [ ] Filter snapshots to the selected problem's dependencies and conflict path.
- [ ] Preserve branch/loop markers.
- [ ] Avoid presenting mutually exclusive events as one runtime execution.
- [ ] Provide a source-derived fallback when compiler snapshots are unavailable.

Acceptance criteria:

- [ ] Borrow creation, conflict, later use, and borrow end appear in order.
- [ ] A branch-specific move displays its branch label.

### FLOW-002 — Build scrubber controls [P0]

- [ ] Add previous/next controls, event ticks, source line, event label, and path
      marker.
- [ ] Support left/right keys and Enter to focus source.
- [ ] Do not add autoplay.

Acceptance criteria:

- [ ] Every step updates without rebuilding topology.
- [ ] Controls fit inside the compact viewport.

### FLOW-003 — Synchronize graph, prose, and editor [P0]

- [ ] Update node states and active edges per step.
- [ ] Highlight the relevant editor range.
- [ ] Show one sentence describing the state change.
- [ ] Keep the selected issue and exact target pinned.

Acceptance criteria:

- [ ] Moving a value visibly transfers its owning edge.
- [ ] Ending a borrow removes or dims the borrow edge and makes the target
      available.
- [ ] Stepping never triggers a new analyzer problem-selection request.

## Milestone 9: Generic method-contract lens

Depends on: RA-004 and RA-005.

### CONTRACT-001 — Render the access contract [P0]

- [ ] Show available access, operation, required access, and result.
- [ ] Show the exact field/deref/borrow route.
- [ ] Show successful compiler-inserted adjustments for non-error calls.

Acceptance criteria:

- [ ] `self.events.push` displays `&self → self.events → push(&mut self, ...)`.
- [ ] A method taking `self` is clearly distinguished from `&self` and
      `&mut self`.

### CONTRACT-002 — Explain operation intent and effects [P0]

- [ ] Show signature-derived ownership facts for every resolved method.
- [ ] Show local body/MIR effects when available.
- [ ] Show documentation-derived intent with separate provenance.
- [ ] State when effect detail is unavailable.

Acceptance criteria:

- [ ] `Vec::clear` explains mutable access and element removal/drop without a
      UI hardcoded branch.
- [ ] A custom local method reports its receiver and observed moves/borrows.

### CONTRACT-003 — Show related operations safely [P1]

- [ ] List relevant documented methods separately from fixes.
- [ ] Include receiver requirement and behavior summary.
- [ ] Do not claim semantic equivalence or compiler validation.

Acceptance criteria:

- [ ] Related methods are labeled `Explore`, not `Fix`.
- [ ] The primary repair list remains compiler-backed.

## Milestone 10: Compiler-validated fix simulator

Depends on: RA-006 and GRAPH-001.

### FIX-001 — Build the primary repair card [P0]

- [ ] Show intent, minimal diff, ownership consequence, mutation consequence,
      thread-safety consequence, runtime risk, and cost.
- [ ] Rank compiler-validated repairs before candidates.
- [ ] Keep candidates in a collapsed alternatives drawer.

Acceptance criteria:

- [ ] Validation status is visually unambiguous.
- [ ] A wrapper suggestion explains behavioral costs rather than merely saying
      it compiles.

### FIX-002 — Build counterfactual preview [P0]

- [ ] Request and cache the temporary compiler validation/model.
- [ ] Display a clearly labeled preview graph without editing source.
- [ ] Highlight changed nodes, edges, contracts, and diagnostics.
- [ ] Allow returning to the original scene.

Acceptance criteria:

- [ ] Preview leaves file bytes and source hash unchanged.
- [ ] A candidate without compiler validation never appears successful.
- [ ] Preview cancellation is immediate after source edits.

### FIX-003 — Apply and revalidate [P0]

- [ ] Apply only after an explicit user action.
- [ ] Re-run compiler/analyzer validation.
- [ ] Report resolved and replacement diagnostics.
- [ ] Clear stale preview/cache state.

Acceptance criteria:

- [ ] The resulting file matches the displayed diff.
- [ ] The sidebar updates to the new compiler result.

### FIX-004 — Compare ownership wrapper choices [P1]

- [ ] Compare `Box`, `Rc`, `Rc<RefCell>`, `Arc`, `Arc<Mutex>`, and
      `Arc<RwLock>` when relevant.
- [ ] Show owner count model, threading, mutation, runtime checking, blocking,
      panic risk, and cost.
- [ ] Render a small topology for each relevant choice.

Acceptance criteria:

- [ ] The comparison never recommends a wrapper solely to silence an error.
- [ ] Irrelevant or invalid wrappers are omitted or explained.

## Milestone 11: Learning polish, C view, and accessibility

Depends on: Milestones 6 through 10.

### LEARN-001 — Enforce the beginner explanation order [P0]

- [ ] Present What happened, involved values, locations, previous valid state,
      operation requirement, conflict, fix change, and tradeoff in that order.
- [ ] Use actual variable names before abstract terms.
- [ ] Move jargon definitions to contextual hover/click content.

Acceptance criteria:

- [ ] No standalone My Concepts or Lessons section appears.
- [ ] A beginner can identify owner, borrower, attempted operation, and conflict
      from the closed core view.

### LEARN-002 — Integrate conceptual C [P1]

- [ ] Synchronize conceptual C with the selected event and graph node.
- [ ] Match Rust variables to conceptual C owner/pointer names.
- [ ] Keep generated C lazy and off the UI thread.
- [ ] Retain warnings about invalid Rust and non-idiomatic generated C.

Acceptance criteria:

- [ ] Conceptual C is labeled as intent, not an ABI-equivalent translation.
- [ ] Generated C is never attempted for stale or invalid saved input.

### LEARN-003 — Complete accessibility support [P0]

- [ ] Add keyboard navigation to header, issue list, graph, scrubber, drawers,
      and fixes.
- [ ] Add accessible names for nodes, edges, states, and controls.
- [ ] Use shape/label differences in addition to color.
- [ ] Respect reduced-motion settings.
- [ ] Verify dark, light, and high-contrast themes.

Acceptance criteria:

- [ ] The primary flow is operable without a mouse.
- [ ] The graph remains understandable in monochrome.
- [ ] At 140% font, content scrolls without overlap or clipping.

### LEARN-004 — Complete responsive layout [P0]

- [ ] Test 480×720, 560×800, and 700×900 panel sizes.
- [ ] Test 80%, 100%, 120%, and 140% sidebar font sizes.
- [ ] Handle long variable names and nested generic types.

Acceptance criteria:

- [ ] At 560×800 and 100%, the closed core requires no vertical scroll.
- [ ] At smaller/larger-font configurations, ordinary scrolling works and all
      controls remain reachable.

## Milestone 12: Comprehensive correctness and interaction tests

Depends on: All P0 implementation milestones.

### TEST-001 — Add deterministic scene snapshots [P0]

- [ ] Snapshot move, copy, deep clone, `Rc` clone, partial move, borrow conflict,
      immutable mutation, lifetime escape, RefCell, lock, Weak, raw pointer,
      unknown wrapper, branch conflict, and truncation scenes.
- [ ] Assert selected place, nodes, edges, state transitions, prose, provenance,
      and repair classification.

Acceptance criteria:

- [ ] Snapshots contain no nondeterministic IDs, order, or geometry.
- [ ] Changes require an intentional review of semantic differences.

### TEST-002 — Add GPUI interaction coverage [P0]

- [ ] Test issue navigation and issue popover.
- [ ] Test graph hover/click/keyboard behavior.
- [ ] Test scrubber mouse/keyboard behavior.
- [ ] Test drawers, font scale, profiles, clues, repair preview/apply, refresh,
      source edits, close, and reopen.

Acceptance criteria:

- [ ] Tests use `TestAppContext`/`VisualTestContext` where appropriate.
- [ ] Interaction tests assert model state, not only rendered text.

### TEST-003 — Lock exact-target regressions [P0]

- [ ] Test E0596 `self.events.push` through initial selection, graph node click,
      scrubber step, inline clue click, repair preview, and refresh.
- [ ] Test `prefix` borrowing from `current` through the same interactions.
- [ ] Test multiple errors in one file and rapid next/previous navigation.

Acceptance criteria:

- [ ] `self.events` never becomes `self` unless the user explicitly selects a
      separate diagnostic whose target is `self`.
- [ ] Stale responses are ignored in every interaction path.

### TEST-004 — Test false-claim prevention [P0]

- [ ] Test unknown third-party wrappers.
- [ ] Test generic and unsized types.
- [ ] Test conditional `Option`, `Result`, and branch states.
- [ ] Test runtime counts, capacities, borrow flags, locks, and addresses remain
      unknown.

Acceptance criteria:

- [ ] Every unavailable fact says unknown/conceptual rather than displaying a
      fabricated value.

### TEST-005 — Run end-to-end stress scenarios [P0]

- [ ] Open `analytics.rs` and inspect every diagnostic.
- [ ] Run move/partial move, wrapper repair, valid pattern, and unsupported
      cases.
- [ ] Verify at least five problems per selected stress file where intended.
- [ ] Verify problem, flow, contract, fix, and result differ appropriately per
      diagnostic.

Acceptance criteria:

- [ ] `./workbench test quick` passes.
- [ ] `./workbench test full` passes.
- [ ] The intentionally failing stress lab remains intentionally failing with at
      least 40 compiler diagnostics overall.

## Milestone 13: Performance, responsiveness, and memory

Depends on: Milestone 12.

### PERF-001 — Move expensive work off the UI thread [P0]

- [ ] Perform artifact parsing, graph derivation, full-function topology,
      repair compilation, layout queries, C generation, and hidden tooltip
      construction outside render.
- [ ] Pass immutable cached scenes to GPUI.
- [ ] Ensure scrubber steps update state only, not topology.

Acceptance criteria:

- [ ] No file I/O, compiler invocation, or full graph construction occurs in
      the panel render call.
- [ ] Instrumentation finds no ownership workbench UI task above 8 ms.

### PERF-002 — Add bounded caches and cancellation [P0]

- [ ] Limit compact scenes to 32.
- [ ] Limit full-function graphs to 8.
- [ ] Limit repair previews to 8.
- [ ] Invalidate by URI/source hash/artifact revision/problem ID.
- [ ] Cancel work after edits, selection changes, close, or newer artifacts.

Acceptance criteria:

- [ ] Repeated navigation through 50 errors reaches a stable memory plateau.
- [ ] Closing the panel releases graph and repair-preview cache memory.

### PERF-003 — Add UI-side benchmarks [P0]

- [ ] Benchmark scene derivation, scrubber step, cached issue switch, inline
      filtering, full map construction, and repair-preview state swap.
- [ ] Use deterministic synthetic and stress-lab models.
- [ ] Save machine-readable results.

Acceptance criteria:

- [ ] Compact scene derivation is below 2 ms p95.
- [ ] Scrubber step is below 4 ms p95.
- [ ] Cached issue switching is below 16 ms p95.
- [ ] Visible-range inline filtering is below 4 ms p95.

### PERF-004 — Strengthen analyzer/compiler performance gates [P0]

- [ ] Update `benchmark-rust-ownership-check` for schema 6.
- [ ] Add baseline JSON comparison instead of relying only on the existing loose
      absolute limits.
- [ ] Extend learning-context benchmarks to graph/access requests.
- [ ] Continue checking artifact byte limits and peak analyzer RSS.

Acceptance criteria:

- [ ] Compiler/analyzer median and p95 regress by no more than 5% from the saved
      `c5295eda4` baseline.
- [ ] No new ownership-artifact event-loop stall exceeds 100 ms.
- [ ] Artifact growth is explained and remains below the configured size gate.

### PERF-005 — Run the final performance and memory pass [P0]

- [ ] Run `./workbench test performance` at least three times.
- [ ] Run the UI benchmark suite at least three times.
- [ ] Run `./workbench disk` with the stress project open.
- [ ] Inspect analyzer/editor logs for stalls, retries, and repeated requests.

Acceptance criteria:

- [ ] All threshold gates pass.
- [ ] No unbounded RSS growth occurs during a 15-minute navigation session.
- [ ] The panel feels as responsive as baseline during typing and issue
      switching.

## Milestone 14: Documentation, build, and delivery

Depends on: Milestone 13.

### DOC-001 — Document the learning workflow [P1]

- [ ] Document opening the workbench and selecting an error.
- [ ] Document graph nodes, arrows, states, and provenance.
- [ ] Document the scrubber, contract lens, repair preview/apply, and C views.
- [ ] Document every inline clue mode/category and its settings.
- [ ] Document source-level storage versus optimized physical placement.
- [ ] Document limitations and unsupported/unknown cases.

Acceptance criteria:

- [ ] Documentation follows `zed-rust/docs/AGENTS.md`.
- [ ] Keybindings use documentation action references rather than hardcoded
      shortcuts.
- [ ] `cd zed-rust/docs && npx prettier --write src/` has been run.
- [ ] `cd zed-rust/docs && npx prettier --check src/` passes.

### DOC-002 — Add a repeatable manual test guide [P1]

- [ ] List exact stress-lab files, cursor positions, expected graph, expected
      contract, and expected repair state.
- [ ] Include dark/light, panel-size, font-size, and keyboard test matrices.
- [ ] Include expected log/performance observations.

Acceptance criteria:

- [ ] A developer unfamiliar with the implementation can reproduce every
      release acceptance scenario.

### SHIP-001 — Run formatting and static checks [P0]

- [ ] Run relevant rustc formatting/checks.
- [ ] Run rust-analyzer formatting/checks.
- [ ] Run Zed formatting/checks.
- [ ] Run `git diff --check`.

Acceptance criteria:

- [ ] No formatting, clippy/static, or whitespace errors remain.

### SHIP-002 — Run full qualification [P0]

- [ ] Run `./workbench doctor`.
- [ ] Run `./workbench build all --portable`.
- [ ] Run `./workbench test quick`.
- [ ] Run `./workbench test full`.
- [ ] Run `./workbench test performance`.

Acceptance criteria:

- [ ] Every command exits successfully.
- [ ] The produced editor uses the intended custom compiler and analyzer.

### SHIP-003 — Package and verify [P1]

- [ ] Run `./workbench package linux`.
- [ ] Run `./workbench test bundle --archive <archive>`.
- [ ] Run the optional clean-container bundle gate when Docker permission is
      available.
- [ ] Record archive path, size, SHA-256, glibc requirement, and test result.

Acceptance criteria:

- [ ] The bundle contains no machine-specific absolute runtime path.
- [ ] Bundle doctor passes.
- [ ] Bundled Cargo/rustc/analyzer/editor run from the extracted bundle.
- [ ] Custom ownership suggestions and workbench views appear in the bundle.

### SHIP-004 — Final local review and commits [P0]

- [ ] Review the diff for unrelated changes.
- [ ] Commit by subsystem: compiler schema, analyzer, editor hints, sidebar,
      learning interactions, performance/tests/docs.
- [ ] Confirm rollback tag remains intact.
- [ ] Leave the remote untouched.

Acceptance criteria:

- [ ] `git status --short` is clean after final local commits.
- [ ] Each commit is independently understandable and bisectable.
- [ ] Nothing is pushed to GitHub.

## First-release definition of done

- [ ] Every P0 and P1 task above is complete.
- [ ] The sidebar's closed state fits at 560×800 and 100% font without
      scrolling.
- [ ] `self.events` exact-target regressions pass through every interaction.
- [ ] Relevant variables, handles, references, wrapper gates, and heap values
      are visible in the main topology.
- [ ] The source-event scrubber explains ownership state changes in order.
- [ ] Layout, storage, access, and wrapper hints can be independently enabled or
      disabled.
- [ ] Compiler-validated fix previews show both code and topology changes.
- [ ] Unknown and runtime-only facts are never fabricated.
- [ ] Correctness, full, performance, memory, accessibility, documentation, and
      bundle gates pass.
- [ ] Final changes remain local and unpushed.

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
| 2026-08-18 | `c5295eda4` | Baseline performance | `./workbench test performance` (three runs); `./workbench disk` | [`evidence/visual-workbench/baseline-performance.json`](evidence/visual-workbench/baseline-performance.json) | Passed; live editor RSS pending |
|  |  | Final correctness | `./workbench test full` |  | Pending |
|  |  | Final performance | `./workbench test performance` |  | Pending |
|  |  | Bundle verification | `./workbench test bundle --archive …` |  | Pending |
