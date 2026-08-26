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
Building needs substantial disk space and can take a long time because it
builds a Rust compiler, rust-analyzer, and Zed.

```bash
scripts/build-all
scripts/run-workbench zed-rust/rust-ownership-stress-lab
```

Run a second isolated editor process with:

```bash
scripts/run-workbench --new-instance zed-rust/rust-ownership-stress-lab
```

Create and verify a distributable archive with:

```bash
scripts/build-release
scripts/verify-release
```

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
