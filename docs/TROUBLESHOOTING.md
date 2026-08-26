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

## Performance gate failure

Run `./workbench test performance` again only after confirming the machine is not
under unrelated CPU, memory, or I/O load. The compiler transport suite runs
three times and enforces the checked-in baseline. A consistent regression must
be fixed; do not update the baseline merely to make the gate pass.

## Release does not start on another machine

The target must be Linux x86-64 with glibc 2.43 or newer, a Vulkan host driver,
and X11 or Wayland. Run the bundled `--doctor` before starting the GUI. The
archive is relocatable and paths containing spaces are covered by verification.
