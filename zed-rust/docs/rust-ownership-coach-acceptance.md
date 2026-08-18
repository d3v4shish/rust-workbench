# Rust Ownership Coach acceptance contract

The coach uses one beginner-first vertical flow. There are no Visual/Guided/My Concepts modes,
lesson launcher, checkpoints, or progress state.

## Correctness

- The red banner appears only for compiler-reported, supported Rust diagnostics from the structured
  `rust-analyzer/ownershipProblems` endpoint.
- Selecting an issue requests the ownership model at its compiler-correlated position and keeps the
  binding, cause, and rejected-use source ranges clickable.
- Schema 4 compiler artifacts record a move/copy destination when MIR proves it: a named binding,
  projected place, return value, or call argument. Rust-analyzer continues to read schemas 2 and 3.
- The analyzer exposes a bounded schema 10 `valueTrace` plus a structured mutation requirement.
  Move, Copy, borrow, clone, reinitialize,
  rejected use, and drop have different states and explanations.
- A move is described as an ownership transfer, not a physical heap copy. `Rc`/`Arc` moves leave the
  symbolic strong count unchanged; their resolved `clone` operations show symbolic `N + 1`. The UI
  never claims to have sampled a runtime count.
- Actual compiler/source facts and hypothetical repair topologies have explicit labels. A repair is
  not shown as successful until a fresh Cargo/rustc scan removes the selected diagnostic.
- Repairs are addressed by stable ID, recomputed by rust-analyzer, rejected for stale source hashes,
  applied through Zed's edit history, and therefore one-step undoable.
- A borrowed value remains alive in every diagram. The UI marks the incompatible access as blocked;
  it does not describe the referent as dead.
- C output remains collapsed under advanced evidence and is labeled conceptual rather than
  ABI-equivalent or guaranteed-to-compile C.

## Beginner flow

The default path reads from top to bottom:

1. **What happened** — rustc message, protected rule, source jump, and precision.
2. **Where the value went** — a bounded, clickable source journey with source/destination state and
   allocation effect.
3. **Three pictures** — Before (actual), Conflict (actual), and Repair target (hypothetical until
   verified).
4. **Pointers and ownership right now** — separate owner/reference/referent nodes plus stack/heap or
   guard layers.
5. **Why the operation needs that access** — resolved receiver signature, behavior, effects, and
   materially different alternatives.
6. **Core rule and intent** — plain language, reason, memory model, misconception, and the
   borrow/clone/Rc/RefCell/Arc/lock intent matrix.
7. **Fixes and result** — intent-level choices, compiler-validated diffs, semantic/runtime costs,
   apply/undo, and compiler verification.
8. **Codebase context** — bounded breadcrumbs, call paths, and related types.

The only secondary disclosure is **Advanced compiler evidence**, collapsed by default. It contains
MIR events and coordinates, loans, representation layers, operation provenance, and C comparison.

## Required visual models

- `String`/`Vec`: stack handle → heap buffer; moving the handle leaves the buffer in place.
- `Box<T>`: unique stack handle → one heap allocation.
- `&T`/`&mut T`: non-owning pointer → live referent; shared versus exclusive access is explicit.
- `Rc<T>`/`Arc<T>`: handle → shared allocation → inner value; count is symbolic.
- `RefCell<T>`: owner → runtime borrow flag → inner value; conflicting access may panic.
- `Rc<RefCell<T>>`: shared ownership and runtime-checked mutation are separate layers.
- `Arc<Mutex<T>>`/`Arc<RwLock<T>>`: atomic shared ownership and synchronization are separate layers.

## Performance

- Compiler events remain capped at 4,096 per body. The interactive response is capped at 256
  events, 24 value-trace steps, 512 blocks, 64 bindings, 64 loans, and bounded loan points.
- The beginner UI renders at most 10 value steps at once and states when more are hidden.
- Artifact JSON parsing remains on a rust-analyzer worker and exact per-file artifacts are committed
  in bulk through persistent cache values.
- Editing is debounced; LSP, compiler, Cargo, filesystem, and generated-C work stays off the UI
  thread.
- `script/benchmark-rust-learning-context` requires schema 10, a non-empty value trace for
  ownership-flow cases or a resolved operation and mutation requirement for mutation cases, zero
  ownership artifact event-loop stalls, warm p95 ≤ 20 ms, and maximum ≤ 100 ms across 100 requests.

## Qualification

Run `./script/qualify-rust-workbench --quick` during development. Before handoff run
`./script/qualify-rust-workbench --full`, launch the release build, and inspect the log while editing,
scrolling, switching among issues, previewing, applying, and undoing a repair. No repeated
ownership-related `overly long loop turn` entry is acceptable.
