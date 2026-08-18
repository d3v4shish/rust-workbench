# Ownership Workbench examples

Launch this directory with the repository's isolated editor:

```sh
./workbench run zed-rust/rust-workbench-example
```

Choose **Trust and Continue**, open a file in `src/bin`, wait for the check to
finish, put the cursor on the variable named by the diagnostic, and press
`Ctrl+Alt+O`. See [`../RUST_WORKBENCH.md`](../RUST_WORKBENCH.md) for expected
timelines and repair families.

The `tutorial_*.rs` binaries form a guided progression from moves and NLL to
`Rc<RefCell<_>>` and `Arc<Mutex<_>>`. Switch the Ownership Studio header from
**Inspect Code** to **Lessons** for the matching explanation sequence.
