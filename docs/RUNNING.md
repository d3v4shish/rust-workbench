# Running

Start the built editor with the stress project:

```bash
scripts/run-workbench zed-rust/rust-ownership-stress-lab
```

Any file or directory arguments after the command are forwarded to the editor.
Use `./workbench run --debug` to launch a debug editor build.

## Instance isolation

Each running editor owns a separate profile, database, log directory, cache,
Cargo analysis target, and compiler ownership-model namespace. The launcher
uses an advisory file lock so two processes never open the same writable state.

The first process uses `default`. Concurrent automatic processes use
`instance-2` through `instance-16`:

```bash
scripts/run-workbench --new-instance path/to/project
scripts/run-workbench --instance teaching-demo path/to/project
```

`--new-instance` creates a timestamped profile. A named profile persists across
runs and fails clearly if that profile is already open. New profiles inherit
the default profile's settings and keymap once, then diverge independently.

Development profiles live under `zed-rust/rust-workbench-data`. Bundle profiles
live under `${XDG_DATA_HOME:-$HOME/.local/share}/rust-workbench`, unless
`RUST_WORKBENCH_DATA_DIR` overrides the root.

## Privacy and local diagnostics

Metrics and diagnostic uploads default to disabled. Launchers remove the
minidump upload endpoint and enable local minidump generation. Logs, databases,
and crash evidence remain inside the selected profile. Treat diagnostic bundles
as potentially sensitive because source paths and compiler messages can contain
project information.

Create a redacted, size-bounded archive of local logs with:

```bash
./workbench diagnostics collect
```

Minidumps are excluded unless `--include-minidumps` is passed explicitly. Preview
removal of diagnostic files older than 30 days with
`./workbench diagnostics prune --dry-run`; omit `--dry-run` to remove them.
