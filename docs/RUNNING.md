# Running

Start the built editor with the stress project:

```bash
scripts/run-workbench zed-rust/rust-ownership-stress-lab
```

Any file or directory arguments after the command are forwarded to the editor.
Use `./workbench run --debug` to launch a debug editor build.

For a managed release installation, use the stable command from any directory:

```bash
rust-workbench --doctor
rust-workbench path/to/project
rust-workbench --new-instance path/to/project
rust-workbench --instance teaching-demo path/to/project
```

The command resolves `~/.local/opt/rust-workbench/current` at runtime, so an
upgrade or rollback does not require changing desktop entries, shell aliases,
or project configuration. If `~/.local/bin` is not already on `PATH`, invoke
`$HOME/.local/bin/rust-workbench` or add that standard user command directory
to the shell profile.

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

Ordinary application uninstall preserves that profile root. Only
`rust-workbench-uninstall uninstall --purge-data` removes it, and the installer
refuses to purge a directory that it did not mark as Rust Workbench data.

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
