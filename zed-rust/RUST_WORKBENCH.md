# Rust Ownership Workbench

Rust Ownership Workbench is a side-by-side Zed build backed by the patched
compiler and rust-analyzer in this source tree. It keeps its own application
identity, settings, cache, and user data, so the normal Zed installation is not
modified. Its red Zed icon distinguishes it from the normal editor in the dock,
task switcher, window decorations, and About dialog.

## Build and launch

From `/home/d3v/Workspace/Temp/RustC/zed-rust`:

```sh
# Required once on a clean Debian/Ubuntu checkout. This is rootless.
./script/bootstrap-rust-workbench-linux

./script/build-rust-workbench
./script/install-rust-workbench-desktop
./script/rust-workbench rust-workbench-example
```

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
lifetimes, memory layouts, MIR evidence, and the C comparison. Clicking a trace
or loan item highlights and scrolls to its Rust source range.

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

Repairs are alternatives, not unconditional recommendations. Each displayed
repair includes:

- the wrapper and access rewrite as a source diff;
- a **compiler validated** marker, meaning the rewritten candidate was
  recompiled and cleared the targeted borrow-checker error;
- a runtime-semantics warning for choices such as `RefCell`, `Rc`, `Arc`,
  `Mutex`, or `RwLock`;
- an **Apply** button that uses Zed's normal workspace edit and undo history.

Compiler validation proves that the candidate type-checks; it does not decide
whether runtime borrow checks, reference counting, locking, or thread-safe
sharing are appropriate for the program's design.

## Quick tests

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

## Development checks

The relevant local checks are:

```sh
cd /home/d3v/Workspace/Temp/RustC/rust
./x check compiler/rustc_borrowck compiler/rustc_driver_impl
./x test tests/run-make/borrowck-autofix --force-rerun

cd /home/d3v/Workspace/Temp/RustC/rust/src/tools/rust-analyzer
cargo test -p rust-analyzer ownership --lib --bins

cd /home/d3v/Workspace/Temp/RustC/zed-rust
./script/clippy -p rust_workbench
./script/build-rust-workbench
```
