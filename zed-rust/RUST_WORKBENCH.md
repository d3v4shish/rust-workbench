# Rust Ownership Workbench

Rust Ownership Workbench is a side-by-side Zed build backed by the patched
compiler and rust-analyzer in this source tree. It keeps its own application
identity, settings, cache, and user data, so the normal Zed installation is not
modified. Its red Zed icon distinguishes it from the normal editor in the dock,
task switcher, window decorations, and About dialog.

## Build and launch

From the combined repository root (the directory containing `workbench.toml`):

```sh
# Required once on a clean Debian/Ubuntu checkout. This is rootless.
./workbench bootstrap
./workbench build all
./zed-rust/script/install-rust-workbench-desktop
./workbench run zed-rust/rust-workbench-example
```

`bootstrap` installs the rootless native build dependencies used by the editor
and bundled Rust toolchain. Run `./workbench doctor` to verify the workspace.

The normal build and desktop launcher use the optimized release binary. For
debugging only, use `./script/build-rust-workbench --debug` followed by
`./script/rust-workbench --debug rust-workbench-example`; the debug editor is
substantially larger and is not representative for responsiveness testing.
The bundled settings likewise point at `rust-analyzer`'s release binary; build
it with `cargo build --release -p rust-analyzer --bin rust-analyzer` after
changing analyzer code.

On the first launch, choose **Trust and Continue** for the bundled example.
Zed intentionally does not run a workspace's tools until it is trusted.

The workbench uses these shortcuts:

- `Ctrl+Alt+O`: show or hide the Rust Ownership Workbench panel.
- `Ctrl+Alt+R`: refresh the compiler ownership model for the current cursor.
- `Ctrl+.`: show ordinary rust-analyzer code actions.
- Normal Zed undo (`Ctrl+Z`) reverses a repair applied from the workbench.

The default panel is a compact guided path: **What went wrong**, **Follow the
value**, **Why Rust stops this**, and **Fix it safely**. Only the selected value
step is expanded. Repair diffs and runtime trade-offs appear only after
Preview. **Explore deeper (optional)** contains resolved calls, timelines,
lifetimes, memory layouts, MIR evidence, and Conceptual C. Clicking a trace
or loan item highlights and scrolls to its Rust source range.

After that local explanation, **Workspace cause and impact** shows one selected
root in a vertical tree. Single-file impact stays collapsed until requested:

- **ROOT** is the move, borrow, or rejected access that established the
  compiler constraint;
- **SELECTED** is the diagnostic you clicked, even when it is a later symptom;
- **CALLER** entries are resolved local call sites that a signature repair may
  affect, not claims that those callers already contain an error;
- **Other workspace roots** is collapsed by default and opens a bounded list of
  unrelated ownership errors in the loaded workspace.

Click any row to open its exact source file and line. The evidence footer says
whether the connection is compiler-exact, diagnostic-backed, resolved from the
call graph, or estimated. Dependency bodies are not expanded.

Some errors then ask one design question, such as whether the original caller
still needs a moved value or who should be allowed to mutate a field. The
answer ranks the repair cards; it never hides the other designs and never
unlocks Apply by itself.

The source is saved after 750 ms of inactivity. Wait for `cargo check` to
finish, put the cursor on the relevant variable, and open the panel. A fresh
result says **Compiler exact**. If the file changed after compilation, the
stale model is discarded instead of presenting it as current truth.

## What the panel shows

The patched compiler writes a versioned ownership-model artifact containing
MIR body IDs, basic blocks, statements, projected places, loan IDs, and states
such as move, partial move, shared/mutable borrow, borrow end, reinitialization,
invalid use, last use, and drop. rust-analyzer maps the compiler byte ranges to
the editor and the native panel presents a cursor-scoped timeline.

The primary memory map follows the compiler's explicit representation chain.
For example, `values: Rc<RefCell<Vec<i32>>>` is shown as variable -> inline Rc
handle -> shared heap allocation -> RefCell access gate -> inline Vec header ->
heap element buffer. Moves reconnect the destination handle to the same
allocation; `Rc::clone` keeps two handles converging on one allocation.

Repairs are alternatives, not unconditional recommendations. Each displayed
repair includes:

- the wrapper and access rewrite as a per-file source diff, plus the complete
  affected-file list;
- a **compiler validated** marker, meaning the rewritten candidate was
  recompiled and cleared the targeted borrow-checker error;
- a runtime-semantics warning for choices such as `RefCell`, `Rc`, `Arc`,
  `Mutex`, or `RwLock`;
- an **Apply** button that uses Zed's normal workspace edit and undo history.

Apply remains hidden if any edited file could not be loaded into the preview,
even when an older analyzer response says the candidate was validated. This is
intentional fail-closed behavior for multi-file code actions.

Compiler validation proves that the candidate type-checks; it does not decide
whether runtime borrow checks, reference counting, locking, or thread-safe
sharing are appropriate for the program's design.

## Conceptual C

Open **Explore deeper (optional)** and then **C intent** to see the selected Rust
ownership event expressed with C-like owner and pointer names. This view explains
intent only. It is neither generated compiler output nor an ABI-equivalent
translation, and it never runs an additional compiler backend.

## Quick tests

### On-demand method coach

Ownership-relevant calls now end with one compact clue such as
`Explain · needs &mut · moves event`.

1. Hover the compact clue, not the method name. The anchored card follows the
   real resolved signature and shows the receiver, each argument, whether it is
   borrowed/copied/moved, what remains usable, and the return path.
2. Click the clue to open the Rust guide at that exact call. A valid call stays
   in operation focus even when another compiler error exists elsewhere in the
   file.
3. Open `Clues` in the guide and toggle `Method coach` to show or hide these
   clues independently.

Ordinary hover over a method name remains the standard rust-analyzer API hover.
Metadata-only reads such as `len`, `is_empty`, and `capacity` are intentionally
omitted. Detailed prose is produced only by `inlayHint/resolve` after a learner
hovers a clue, so visible-range updates carry only a compact label and source
coordinates.

Try this small call-boundary example:

```rust
fn main() {
    let mut events = Vec::<String>::new();
    let event = String::from("signed-in");
    events.push(event);
    let latest = events.last();
    println!("{latest:?}");
}
```

The `push` card should show a temporary mutable borrow of `events` and a move of
`event`; the `last` card should show a returned shared reference tied to
`events`. There should be no method-coach clue on `len()` in an equivalent
example.

Open one file at a time under `rust-workbench-example/src/bin`, wait for the
diagnostic, place the cursor on the named variable, then press `Ctrl+Alt+O`:

| File | Trigger | Expected repair family |
| --- | --- | --- |
| `box_to_rc.rs` | `values` is moved and then used | `Rc` and `Arc` shared ownership alternatives |
| `refcell.rs` | immutable `Vec` is mutated | `RefCell`, `Mutex`, and `RwLock` alternatives |
| `box_rc_refcell.rs` | immutable `Box<Vec<_>>` is mutated | interior-mutability alternatives with the existing box removed as needed |
| `rc_refcell.rs` | data behind `Rc<Vec<_>>` is mutated | `Rc<RefCell<_>>` and thread-safe lock alternatives |
| `partial_move.rs` | one field is moved, then reused | field-level partial-move timeline; repairs may be absent because whole-struct wrapper rewrites are intentionally conservative |

For the first example, a correct fresh panel includes the compiler-exact move
location, the rejected use-after-move, MIR locations, and one or more repair
cards with visible diffs. Apply a repair, verify the error disappears, and use
`Ctrl+Z` to restore the original program.

For a workspace-wide test, open
`rust-ownership-stress-lab/crates/commerce_app/src/analytics.rs`. Select the
`current` E0506 diagnostic or the `self.events.push` E0596 diagnostic and open
the guide. Confirm that:

1. the root card names the complete place (`current`/`prefix` or
   `self.events.push`), not only `self`;
2. the clicked red diagnostic remains marked **SELECTED**;
3. **Other workspace roots** lists errors from the other stress-lab files;
4. choosing an intent changes the first repair while **Other designs** remains
   available;
5. a repair preview reports its edit scope, and Apply appears only for a
   compiler-validated, complete preview.

## Development checks

The relevant local checks are:

```sh
./workbench test quick
./workbench test full
./workbench test performance
./workbench package linux
```
