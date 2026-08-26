# Building

## Supported host

The release target is Ubuntu 26.04, Linux x86-64, glibc 2.43 or newer. The
bootstrap script requires `apt-get` and `dpkg-deb`. It downloads Debian packages
and extracts them into `zed-rust/.build-deps/sysroot`; it does not install
system packages or use `sudo`.

Required host commands include Git, Python 3.11 or newer, Cargo, `apt-get`,
`dpkg-deb`, `readelf`, `tar`, `zstd`, `flock`, and standard GNU utilities.
Network access is needed for the first bootstrap and any uncached Cargo or
WebRTC dependency.

## Complete development build

```bash
scripts/build-all
```

That command bootstraps local native dependencies, builds stage1 rustc and
rustdoc, builds rust-analyzer, builds the release editor, and runs the workspace
doctor. Important outputs are printed when the command completes.

The equivalent lower-level commands are:

```bash
./workbench bootstrap
./workbench build all
./workbench doctor
```

Individual components can be rebuilt with `./workbench build compiler`,
`./workbench build analyzer`, or `./workbench build editor`.

## Qualification

Use the smallest relevant gate while iterating, then run the complete set before
a release:

```bash
./workbench test resilience
./workbench test ui
./workbench test multi-instance
./workbench test performance
./workbench test quick
./workbench test full
```

`quick` covers analyzer, Zed integration, the intentionally broken stress lab,
and touched-crate compilation. `full` additionally runs compiler rewrite tests,
rebuilds a coherent stage1 toolchain, builds the release editor, and runs the
compiler-backed integration benchmarks.

## Generated files

Generated outputs are intentionally outside source control. Inspect their size
with `./workbench disk`. Preview cleanup with `./workbench clean --dry-run`,
remove debug caches with `./workbench clean --debug-caches`, or remove all
configured generated outputs with `./workbench clean --all`.
