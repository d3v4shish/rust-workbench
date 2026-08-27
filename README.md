# Rust Workbench

Rust Workbench is a compiler-backed Rust learning build of Zed. It connects a
custom Rust compiler, rust-analyzer, and a visual side panel that explains
ownership, borrowing, wrappers, heap storage, async state, and trait objects
using the source issue currently selected in the editor.

The repository is a monorepo. The primary source trees are:

- `rust/`: the custom compiler and rust-analyzer source.
- `zed-rust/`: the Zed fork and Rust learning panel.
- `tools/`: build, packaging, and relocatable bundle support.
- `scripts/`: stable commands intended for developers and releases.

## Quick start

The supported build host is Ubuntu 26.04 on x86-64 with glibc 2.43 or newer.
Building needs 300 GiB of free disk space and can take a long time because it
builds a Rust compiler, rust-analyzer, and Zed. Inspect the exact host package
requirements before starting:

```bash
./workbench prerequisites build
./workbench prerequisites build --format apt
scripts/build-all
scripts/run-workbench zed-rust/rust-ownership-stress-lab
```

Run a second isolated editor process with:

```bash
scripts/run-workbench --new-instance zed-rust/rust-ownership-stress-lab
```

Create and verify a distributable archive with:

```bash
./workbench prerequisites vm --format apt
scripts/build-release
```

`scripts/build-release` includes the mandatory disposable Ubuntu 26.04 KVM
installation test. It does not publish or change Git tags.

## Install a release

On an Ubuntu 26.04 x86-64 desktop, verify and extract the three assets from the
private GitHub Release, then run the managed user installer:

```bash
sha256sum --check rust-workbench-1.1.0-linux-x86_64-glibc2.43.tar.zst.sha256
tar --zstd -xf rust-workbench-1.1.0-linux-x86_64-glibc2.43.tar.zst
rust-workbench.app/bin/rust-workbench --doctor
rust-workbench.app/bin/install-user install
rust-workbench --doctor
rust-workbench path/to/rust/project
```

The default application root is `~/.local/opt/rust-workbench`; the stable
command is `~/.local/bin/rust-workbench`. No repository checkout or fixed
extraction directory is required after installation. See
[Packaging](docs/PACKAGING.md) for updates, rollback, custom prefixes, and
uninstall behavior.

## Documentation

- [Building](docs/BUILDING.md)
- [Running and profiles](docs/RUNNING.md)
- [Packaging and releases](docs/PACKAGING.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Troubleshooting](docs/TROUBLESHOOTING.md)
- [Licensing](docs/LICENSING.md)

There is intentionally no CI/CD configuration. Qualification and release
creation are explicit local commands. Build artifacts remain ignored by Git;
the release archive, checksum, and manifest belong in a GitHub Release rather
than in repository history.
