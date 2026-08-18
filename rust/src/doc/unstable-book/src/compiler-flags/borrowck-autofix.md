# `borrowck-autofix`

The `-Zborrowck-autofix=<path>` compiler flag enables an experimental repair mode for a small
set of borrow-checker diagnostics. The compiler writes patched copies below `<path>`, compiles
through those copies, and never changes the original Rust source files.

The mode currently applies two conservative, type-preserving edits:

- adding `mut` to a simple local binding when that is the machine-applicable repair for E0596;
- cloning a simple local value at a move site when the value is known to implement `Clone`.

The compiler may run analysis repeatedly to validate the patched program. A `repair.json` file in
the per-crate output directory records every applied edit and the final status. At most four repair
rounds are attempted.

The additional `-Zborrowck-autofix-wrapper-variants` flag asks the compiler to investigate a small
set of type-changing alternatives. It requires `-Zborrowck-autofix=<path>`. The minimal repair still
controls the normal compiler output and exit status; wrapper variants are compiled independently
below each crate's `variants` directory and recorded in the version 2 manifest.

The experimental wrapper planner currently handles simple local `let` bindings within one
synchronous function:

- `Box<T>` with a moved local alias can become `Rc<T>`;
- an immutable `T` that needs mutation can become `RefCell<T>`;
- `Box<T>` or `Rc<T>` that needs mutation can become `Rc<RefCell<T>>`.

It rewrites canonical `Box::new` and `Rc::new` constructors, explicit single-argument type
annotations, local aliases, direct method receivers, and direct field/index uses. It uses fully
qualified `std` paths, writes one variant per binding, and writes an `all-wrappers` variant when
multiple bindings qualify. Up to four additional minimal-repair rounds may be attempted for each
variant.

The planner conservatively rejects macros, closures and async bodies, non-identifier patterns,
unknown constructors, arguments and returns that would change an API, and other consuming or
escaping uses. It only considers single-threaded `Rc`/`RefCell`; it never silently substitutes
`Arc`, `Mutex`, or `RwLock`. `RefCell` moves borrow checking to runtime and can panic on conflicting
borrows, while `Rc` changes ownership and allocation behavior, so generated variants must still be
reviewed as design alternatives rather than source-level fixes.

When invoked by Cargo through `RUSTFLAGS`, automatic repair is limited to crates marked as primary
packages. Dependency crates compile normally.
