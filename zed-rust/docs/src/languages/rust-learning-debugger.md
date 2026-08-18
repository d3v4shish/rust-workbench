---
title: Rust Learning Debugger
description: "Inspect compiler-backed ownership, borrowing, memory topology, access contracts, and repair previews in the custom Rust Workbench build."
---

# Rust Learning Debugger

Rust Learning Debugger is part of the custom Rust Workbench build of Zed. It
uses the bundled compiler and rust-analyzer. Stock Zed and stock rust-analyzer
do not provide this panel or its extended protocol.

The panel keeps one compiler diagnostic selected while you inspect its values,
source events, method contract, and repairs. Compiler facts are labeled
`compiler exact`. Conservative source analysis and conceptual standard-library
models have separate labels.

## Open and navigate {#rust-learning-open}

1. Open a Cargo workspace with the custom Rust Workbench build.
2. Open a Rust file and save it. Cargo check supplies exact borrow-checker
   facts.
3. Run {#action rust_workbench::Toggle}.
4. Use **Previous**, **Next**, or **All issues** in the pinned issue row.
5. Select **Show in code** to reveal the active compiler range.

Moving the editor cursor does not silently replace the selected issue. Select a
different issue from the navigator when you want the panel to change context.
Run {#action rust_workbench::Refresh} after a toolchain or workspace change.

## Read the main flow {#rust-learning-flow}

The closed panel follows one vertical sequence:

1. **Problem** names the exact value or projected place and the rejected
   operation.
2. **Rule** explains the protected Rust rule in plain language.
3. **Flow and memory** separates local handles, inline wrappers or gates, and
   pointees or allocations.
4. **Fix and result** shows the source diff, consequences, validation state,
   and post-check result.

The source-event buttons move the code highlight and graph state together. A
move transfers ownership to the destination. A borrow leaves the value alive
but temporarily restricts access. A borrow-end step restores access; it does
not recreate a value. Branch markers identify control-flow-specific events.

### Graph symbols and arrows {#rust-learning-graph}

- `●` means usable on the selected source step.
- `◇` means a live borrow restricts access.
- `→ moved` means the old place no longer owns that value on this path.
- `!` means rustc rejected the attempted operation.
- `owns` connects a unique owner to owned storage.
- `shares allocation` connects `Rc` or `Arc` handles to one control block.
- `borrow shared` and `borrow mutable` are non-owning loans.
- `guards access` connects `RefCell`, lock, or guard state to the protected
  value.
- `weak reference` reaches a control block without keeping the inner value
  alive.

Graph nodes use labels and shapes in addition to color. Selecting a node
highlights its source range without replacing the active compiler problem.
Compact graphs are bounded. A truncation label means more compiler facts exist
than the closed view renders.

## Method access contracts {#rust-learning-contracts}

For a resolved call, the panel shows:

```text
available access -> receiver path -> method requirement -> result
```

For example, `self.events.push(...)` shows the shared `&self` route to
`self.events` and the resolved `push(&mut self, ...)` requirement. The signature
is authoritative. Workspace-body effects, documentation, and the versioned
standard-library behavior catalog have separate provenance.

Related methods appear as exploration choices. They are not fixes and are not
claimed to be behaviorally equivalent.

## Preview and apply repairs {#rust-learning-repairs}

Select **Preview & compiler-check** to inspect a diff and counterfactual
topology. Previewing does not edit the source. A candidate stays yellow until a
fresh compiler check accepts the complete rewrite. The graph still labels
wrapper topology as derived or conceptual because runtime addresses, counts,
borrow flags, and locks are not sampled.

**Apply** appears only for a compiler-validated rewrite. Applying is explicit
and undoable. The panel then checks that the selected diagnostic disappeared
and reports replacement diagnostics instead of hiding them.

`Rc`, `RefCell`, `Arc`, and lock-based designs change program semantics. Prefer
ordinary ownership or borrowing when it matches the intended API. Review
threading, runtime checks, panic or poisoning behavior, cycles, blocking, and
cost before applying a wrapper rewrite.

## Configure editor clues {#rust-learning-clues}

Use **Editor clues** in the panel header, or run
{#action rust_workbench::OpenDisplaySettings}. Modes are:

- **Off**: no layout, storage, access, or wrapper mechanics clues.
- **Selected path**: show clues only on the active ownership path.
- **Configured scope**: use the selected function or file scope.

Layout, storage, dereference/access, and wrapper/gate clues can be switched
independently. Inline compiler diagnostics are controlled separately, so you
can keep compact mechanics clues without rendering full diagnostic text after
every line.

Click a mechanics clue to open the panel at that clue's source path. The panel
unlocks the previous issue, scans the diagnostic at the clue anchor, and keeps
projected places such as `self.events` distinct from their root owner `self`.
Hover text is requested lazily, so hidden or untouched clues do not construct
their long explanation on the editor path.

The bundled rust-analyzer also exposes these opt-in settings to standard LSP
clients:

```json [settings]
{
  "lsp": {
    "rust-analyzer": {
      "initialization_options": {
        "ownership": {
          "enable": true,
          "mechanics": {
            "enable": true,
            "layout": true,
            "storage": true,
            "access": true,
            "wrappers": true
          }
        }
      }
    }
  }
}
```

These settings require the bundled rust-analyzer binary. Other clients render
the clues as ordinary inlay labels even when they ignore the custom semantic
metadata.

## Storage and C views {#rust-learning-storage-c}

Stack, inline, and heap labels describe source-level Rust representation and
ownership semantics. Optimizations may keep values in registers, remove
allocations, or otherwise change physical machine placement. The panel never
shows a runtime address, collection capacity, reference count, lock state, or
`RefCell` borrow flag unless a future explicit runtime-observation mode supplies
that evidence.

The conceptual C drawer explains intent using C-like owner and pointer names.
It is not ABI-equivalent output. Generated C is lazy, uses a saved valid input,
and runs outside the UI thread. Invalid or stale Rust is not presented as a
successful C translation.

## Repeatable acceptance check {#rust-learning-manual-check}

Use `rust-ownership-stress-lab` from the workspace root.

| File and line                       | Expected selected facts                                                                                  |
| ----------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `commerce_app/src/analytics.rs:14`  | E0596; target `self.events`; access source `&self`; `push` needs a mutable borrow                        |
| `commerce_app/src/analytics.rs:22`  | E0506; `prefix` points into `current`; `current` remains alive but mutation conflicts with the later use |
| `commerce_app/src/analytics.rs:28`  | E0596; `clear` requires mutable access and removes/drops elements while retaining capacity               |
| `commerce_app/src/analytics.rs:34`  | E0502; `latest` borrows from the vector while `push` may change its buffer                               |
| `commerce_app/src/analytics.rs:41`  | E0382; ownership of `event` moves into `record_owned` before the later print                             |
| `move_cases/src/box_to_shared.rs`   | old owner to new owner flow; validated `Rc` and `Arc` alternatives remain distinct                       |
| `move_cases/src/partial_profile.rs` | moved field and still-available sibling fields are separate places                                       |
| `valid_patterns/src/rc_refcell.rs`  | one shared allocation, several handles, runtime borrow gate, and no fabricated current count             |
| `valid_patterns/src/arc_mutex.rs`   | thread-safe shared allocation followed by an exclusive lock gate                                         |
| `unsupported_cases/src/lib.rs`      | limited unknown/opaque facts instead of an invented smart-pointer model                                  |

For `analytics.rs`, navigate all five issues with the panel buttons. Confirm
that graph clicks, source steps, preview, and refresh never change
`self.events` to `self`. Confirm that each issue changes the problem, flow,
contract, and fix content.

Repeat at panel sizes 480×720, 560×800, and 700×900, with sidebar font settings
80%, 100%, 120%, and 140%. Check dark, light, and high-contrast themes. At
560×800 and 100%, the closed core should fit without nested scrolling. At other
sizes, one panel scroll must keep every control reachable.

During a performance check, rapidly navigate at least 50 issue changes and
type in the active file. The editor should remain responsive, analyzer memory
should reach a plateau, and logs should not contain repeated ownership artifact
stalls above 100 ms.

Run `./workbench doctor` before testing a proc-macro workspace. A complete
stage-1 toolchain reports both `custom rustdoc` and `stage1 proc-macro server`.
`./workbench build compiler` builds rustc, rustdoc, and the ABI-matched
proc-macro server together so a later bootstrap step cannot prune one of them.

## Limitations {#rust-learning-limitations}

- Exact flow arrives after Cargo check. Before that, conservative facts are
  labeled as analyzer estimates.
- Generic, unsized, conditional, and third-party wrapper internals can remain
  unknown.
- Standard-library container topology describes public semantics, not private
  field layout that may change between Rust versions.
- Repair validation proves that a generated source variant compiled. It does
  not prove that the variant matches your application intent.
- Runtime counts, allocation addresses, locks, and borrow flags are not
  observed by this compile-time view.
