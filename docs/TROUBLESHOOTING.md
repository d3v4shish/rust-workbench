# Troubleshooting

## Start with the doctor

For a source build run `./workbench doctor`. For an extracted release run
`rust-workbench.app/bin/rust-workbench --doctor`. Both report missing tools,
artifacts, ABI constraints, and display availability.

## The panel says rust-analyzer is unavailable

Open a saved Rust file in a Cargo project, wait for rust-analyzer initialization,
then use the panel refresh button. The panel intentionally retains matching
last-known compiler facts during a restart. If the status persists, inspect the
selected profile's logs and confirm the custom analyzer exists with
`./workbench doctor`.

## A second window will not open

A named profile may be locked by another process. Use `--new-instance`, choose a
different `--instance NAME`, or close the process holding that profile. Do not
delete `.instance.lock` while an editor is running; the lock is attached to the
process, not the file's presence.

## The editor opens but the diagram is blank

Select a compiler issue in the issue picker or place the cursor inside its
primary source range, then refresh. A blank visualization after a selected issue
is a defect: preserve the profile logs and local minidump, note the source commit
from `git describe --always`, and rerun `./workbench test resilience` and
`./workbench test ui`. Use `./workbench diagnostics collect` to create a redacted
log archive. Add `--include-minidumps` only when the recipient needs crash state
and the project permits sharing it.

## Packaging rejects the worktree

Release manifests must describe a committed source revision. Commit the intended
source changes and confirm `git status --short` is empty. Generated `dist/`,
`target/`, compiler build, and profile directories are ignored and do not need
to be committed.

## A prerequisite report fails

Use the matching generated package command rather than guessing package names:

```bash
./workbench prerequisites build --format apt
./workbench prerequisites runtime --format apt
./workbench prerequisites vm --format apt
```

The report also fails when free space is below the supported recommendation.
The build scope needs 300 GiB free; an extracted release installation should
have at least 4 GiB free in addition to the extracted application.

## The installer refuses a command path

The managed installer will not overwrite an existing `rust-workbench` or
`rust-workbench-uninstall` command unless it points into a Rust Workbench
managed or recognized legacy installation. Move or rename the unrelated
command, then retry. Desktop integration paths containing a newline, quote,
backslash, or percent sign are rejected because freedesktop `Exec` parsing
would change their meaning.

Run the source bundle's doctor before retrying:

```bash
rust-workbench.app/bin/rust-workbench --doctor
rust-workbench.app/bin/install-user install --dry-run
```

An interrupted or failed upgrade keeps the prior `current` target active. Use
`rust-workbench-uninstall rollback` only when a previous version is listed at
`~/.local/opt/rust-workbench/previous`.

## The desktop icon is missing

Confirm both generated files exist, then sign out and back in if the desktop
shell has retained an old cache:

```bash
test -f ~/.local/share/applications/dev.rustworkbench.EditorDev.desktop
test -f ~/.local/share/icons/hicolor/512x512/apps/dev.rustworkbench.EditorDev.png
update-desktop-database ~/.local/share/applications
```

Re-running `rust-workbench.app/bin/install-user install` refreshes the entry and
icon atomically. Do not edit the installed desktop file; fix the source template
and rebuild when changing application identity.

## The disposable VM gate will not start

Install the generated VM prerequisites, confirm virtualization is enabled, and
confirm the current user can open `/dev/kvm`:

```bash
./workbench prerequisites vm --format apt
test -r /dev/kvm && test -w /dev/kvm
```

Group membership changes may require a logout. Use
`scripts/verify-release --vm --keep-vm-on-failure` to retain the failed overlay,
cloud-init input, serial log, and QEMU log under `.vm-cache/`. The default run
always stops and removes the disposable VM. The checksum-verified Ubuntu base
image is the only retained cache; add `--purge-vm-image` to remove it after a
successful run.

## Performance gate failure

Run `./workbench test performance` again only after confirming the machine is not
under unrelated CPU, memory, or I/O load. The compiler transport suite runs
three times and enforces the checked-in baseline. A consistent regression must
be fixed; do not update the baseline merely to make the gate pass.

## Release does not start on another machine

The target must be Linux x86-64 with glibc 2.43 or newer, a Vulkan host driver,
and X11 or Wayland. Run the bundled `--doctor` before starting the GUI. The
archive and managed install are relocatable and paths containing spaces are
covered by verification. Run the release's checksum before extraction and use
the managed installer instead of moving an already installed payload.
