# Packaging

## Release build

Release packaging requires a clean committed worktree so the manifest and build
receipt identify one exact source revision.

```bash
scripts/build-release
```

The command builds a portable editor and complete compiler toolchain, assembles
the relocatable application, validates ELF dependencies and glibc requirements,
runs compiler and analyzer smoke tests, and verifies the extracted archive.

Version 1.1.0 produces these GitHub Release assets under `dist/`:

```text
rust-workbench-1.1.0-linux-x86_64-glibc2.43.tar.zst
rust-workbench-1.1.0-linux-x86_64-glibc2.43.tar.zst.sha256
rust-workbench-1.1.0-linux-x86_64-glibc2.43.tar.zst.manifest.json
```

Verify an existing asset with `scripts/verify-release`. Add `--container` to
also test in a network-disabled Ubuntu 26.04 container; Docker is required for
that optional gate.

## Installing the archive

```bash
tar --zstd -xf rust-workbench-1.1.0-linux-x86_64-glibc2.43.tar.zst
rust-workbench.app/bin/rust-workbench --doctor
rust-workbench.app/bin/rust-workbench path/to/rust/project
```

The bundle includes rustc, rustdoc, Cargo, rust-analyzer, rust-src, required
non-glibc runtime libraries, and a native build SDK. The host still supplies a
Vulkan driver and an X11 or Wayland display.

## GitHub release flow

Create a private repository, push the source commit and annotated tag
`rust-workbench-v1.1.0`, then upload the three files above as Release assets.
Do not commit the archive to Git. There is no CI/CD workflow in this repository;
the local qualification log is the release evidence.
