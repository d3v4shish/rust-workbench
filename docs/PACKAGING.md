# Packaging And Installation

## Release build

Release packaging requires a clean committed worktree so the manifest, build
receipt, tag, and archive identify one exact source revision. The complete gate
is:

```bash
./workbench prerequisites build
./workbench prerequisites vm
scripts/build-release
```

`scripts/build-release` bootstraps the workspace-local native SDK, builds the
portable editor and complete compiler toolchain, assembles and validates the
relocatable application, tests an extracted copy, and finally launches the
mandatory disposable Ubuntu 26.04 KVM test. It does not create a tag, upload an
asset, install the release on the host, or publish anything.

Version 1.1.0 produces these ignored files under `dist/`:

```text
rust-workbench-1.1.0-linux-x86_64-glibc2.43.tar.zst
rust-workbench-1.1.0-linux-x86_64-glibc2.43.tar.zst.sha256
rust-workbench-1.1.0-linux-x86_64-glibc2.43.tar.zst.manifest.json
```

The archive, checksum, and release manifest are GitHub Release assets. They are
not committed to Git.

## Disposable VM gate

Install the one-time host tooling with the exact command printed by:

```bash
./workbench prerequisites vm --format apt
```

Hardware virtualization and read/write access to `/dev/kvm` are required. The
gate downloads the official Ubuntu 26.04 x86-64 cloud image and verifies it
against the release SHA256SUMS before caching it in `.vm-cache/`. Each run then:

1. Creates a 40 GiB sparse overlay, ephemeral SSH key, and cloud-init seed.
2. Boots a clean KVM guest and installs the declared runtime packages.
3. Copies and checksums the release archive inside the guest.
4. Extracts from a path containing spaces and runs the source-bundle doctor.
5. Performs the managed user install, then deletes the extracted source.
6. Blocks guest outbound network and runs the installed doctor.
7. Exercises rustc, Cargo, rust-analyzer, and the bundled C toolchain.
8. Compiles and runs a Rust crate whose build script compiles native C.
9. Starts the editor with Mesa software Vulkan and waits for `Rendered first frame`.
10. Reboots, reruns the doctor, uninstalls, and verifies user data was preserved.

The QEMU process and disposable files are removed on success or ordinary
failure. The verified base image remains cached. These options are available
for investigation and cache cleanup:

```bash
scripts/verify-release --archive dist/ARCHIVE.tar.zst --vm
scripts/verify-release --vm --keep-vm-on-failure
scripts/verify-release --vm --purge-vm-image
```

Docker verification remains optional and is independent of the VM gate:

```bash
scripts/verify-release --container
```

A release must not be tagged or uploaded when the VM gate has not passed.

## Verify and install an archive

The target is Ubuntu 26.04, Linux x86-64, glibc 2.43 or newer. The host supplies
a Vulkan driver and an X11 or Wayland desktop. Start with at least 4 GiB free in
addition to the extracted application.

Install any missing runtime packages before extracting:

```bash
sudo apt-get update
sudo apt-get install --yes bash coreutils-from-uutils desktop-file-utils \
  libc-bin libvulkan1 sed util-linux
```

`libvulkan1` supplies the loader, not the hardware-specific Vulkan driver. Use
the driver package recommended for the target GPU.

```bash
sha256sum --check rust-workbench-1.1.0-linux-x86_64-glibc2.43.tar.zst.sha256
tar --zstd -xf rust-workbench-1.1.0-linux-x86_64-glibc2.43.tar.zst
rust-workbench.app/bin/rust-workbench --doctor
rust-workbench.app/bin/install-user install
$HOME/.local/bin/rust-workbench --doctor
```

The installer copies a validated payload to
`~/.local/opt/rust-workbench/versions/VERSION-COMMIT`, switches the relative
`current` link atomically, and creates:

```text
~/.local/bin/rust-workbench
~/.local/bin/rust-workbench-uninstall
~/.local/share/applications/dev.rustworkbench.EditorDev.desktop
~/.local/share/icons/hicolor/512x512/apps/dev.rustworkbench.EditorDev.png
```

The extracted `rust-workbench.app` directory can be removed after installation.
The installed payload contains no checkout path or machine-specific home path.

## Update and rollback

Verify and extract a newer archive, then run its installer. The prior payload
is retained as `previous`; older payloads are removed after a successful switch.
Any failed post-switch check restores the prior `current` target.

```bash
new-rust-workbench.app/bin/install-user install
rust-workbench --doctor
rust-workbench-uninstall rollback
```

Rollback swaps `current` and `previous`, validates the target before switching,
and restores the original target if the installed command fails its doctor.

## Custom user paths

Both locations can be selected without modifying the bundle:

```bash
rust-workbench.app/bin/install-user install \
  --prefix "$HOME/Applications/Rust Workbench" \
  --bin-dir "$HOME/bin"
```

Use the same options for rollback or uninstall. `RUST_WORKBENCH_INSTALL_ROOT`
and `RUST_WORKBENCH_BIN_DIR` provide equivalent defaults. Paths with spaces are
supported; paths that cannot be represented safely in a freedesktop `Exec`
field are rejected.

## Uninstall and user data

Ordinary uninstall removes the managed versions, stable commands, desktop
entry, and icon while preserving profiles, settings, logs, and caches:

```bash
rust-workbench-uninstall uninstall
```

Data removal must be explicit:

```bash
rust-workbench-uninstall uninstall --purge-data
```

The purge is allowed only when the directory contains the installer-owned Rust
Workbench marker. `--dry-run` reports the selected source, payload, commands,
desktop files, and data root without changing them.

## GitHub release flow

1. Run the focused gates and `./workbench test full`.
2. Commit the intended source and confirm `git status --short` is empty.
3. Run `scripts/build-release` and retain its successful VM output.
4. Check the `.sha256` sidecar and confirm both manifests name `git rev-parse HEAD`.
5. Create the annotated `rust-workbench-vVERSION` tag on that commit.
6. Push the source branch and tag to the private `origin` repository.
7. Create the matching GitHub Release and upload only the three `dist/` assets.
8. Download the published assets, recheck the checksum, install, and run the doctor.

Published tags and assets are normally immutable. Replacing an existing release
and deliberately moving its tag requires explicit owner approval, a passing VM
gate for the replacement commit, and verification that the remote tag, release
manifest, embedded manifest, and checksum all identify that same commit.

There is intentionally no CI/CD workflow. Local qualification is the release
gate.
