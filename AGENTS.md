# Rust Workbench Agent Guide

This file applies to the complete monorepo. More specific guidance, especially
`zed-rust/.rules`, also applies inside that subtree. Do not replace or bypass
the nested rules.

## Product Intent

Rust Workbench is a compiler-backed Rust learning build of Zed. It connects a
custom rustc, a custom rust-analyzer, the Zed project bridge, and a visual
learning panel. The product should help a programmer coming from another
language understand why Rust rejected an operation and what structural change
would make the program valid.

Preserve these product requirements:

- Compiler facts are the source of truth. The UI must not invent ownership
  state or present an unvalidated repair as applicable.
- Do not expose generated C in the learning UI. The alternate backend may be
  packaged for toolchain use, but it is not part of the explanation flow.
- Explanations combine beginner intuition, the relevant Rust types, focused
  code, and a comparison of the rejected and valid structures.
- Diagrams show concrete topology: binding or field, wrapper layers such as
  `Box`, `Rc`, `Arc`, `RefCell`, `Mutex`, and `RwLock`, then the heap value.
  Specialized views should explain `Vec` buffers and reallocation, borrows,
  async saved state and `await`, and `&dyn`/`Box<dyn>` fat-pointer metadata.
- Color has stable meaning and diagrams remain readable without color. Inline
  explanation surfaces use an opaque background and must not overlap code.
- Selecting an issue focuses its source range. Issue navigation must continue
  through the complete result set, even when the rendered list is paged.
- Compiler, analyzer, and UI failures become bounded error states, not panics.
  Multiple open editor instances must not share writable profiles or analysis
  targets.

## Repository Map

- `rust/`: custom rustc plus the in-tree rust-analyzer source. Borrow-checker
  ownership events live under `rust/compiler/rustc_borrowck`; analyzer
  ownership insight, repairs, compiler transport, and flycheck integration live
  under `rust/src/tools/rust-analyzer`.
- `zed-rust/`: the Zed fork. The learning UI is in
  `zed-rust/crates/rust_workbench`; the validated custom LSP boundary is in
  `zed-rust/crates/project/src/lsp_store/rust_analyzer_ext.rs`.
- `zed-rust/rust-ownership-stress-lab/`: intentionally invalid Rust fixtures
  spanning many diagnostic and ownership categories. These sources and their
  lockfile are test inputs and belong in Git.
- `zed-rust/rust-workbench-example/`: smaller teaching examples. Sources,
  documentation, manifests, and lockfiles belong in Git.
- `tools/workbench.py` and `workbench.toml`: the unified build, test, run,
  packaging, cleanup, compatibility, and release configuration.
- `tools/bundle/`: relocatable bundle launchers, desktop integration, and
  native-tool wrappers. `install-user` owns managed install, update, rollback,
  uninstall, and desktop integration behavior. Durable installation fixes
  belong here, not in a local installed copy.
- `scripts/`: stable human-facing entry points. Prefer these for complete builds
  and releases.
- `docs/`: project build, runtime, architecture, packaging, licensing, and
  troubleshooting documentation.
- `evidence/visual-workbench/`: small, reviewed, redacted qualification records
  and performance baselines. Do not place raw logs or large captures here.

The root is the only Git repository. `rust/` and `zed-rust/` are source trees
inside it, not repositories to push independently.

## Git And Publishing Contract

The private canonical remote is:

```text
origin  git@github.com:d3v4shish/rust-workbench.git
```

`origin/main` is the integration branch. Use a topic branch for work in
progress and push it with `git push -u origin HEAD`. Merge or push to `main`
only when explicitly requested and after the required gates pass. Never force
push, rewrite shared history, delete checkpoint/release tags, or change the
default branch without explicit approval.

### Commit To Git

Commit the source needed to reproduce the product:

- Root orchestration and policy: `AGENTS.md`, `README.md`, `Todo.md`,
  `.gitignore`, `.gitattributes`, `workbench`, `workbench.toml`, `scripts/`,
  `tools/`, and `docs/`.
- Custom compiler, standard-library, compiler test, and rust-analyzer source
  changes under `rust/`.
- Zed source, assets, configuration defaults, project bridge, learning panel,
  launcher source, and focused tests under `zed-rust/`.
- Intentional stress/example Rust source, `Cargo.toml`, `Cargo.lock`, and fixture
  documentation.
- Small deterministic baselines or redacted qualification summaries under
  `evidence/visual-workbench/` when they are intentionally updated and reviewed.
- Lockfile changes caused by deliberate dependency or package-version changes.

Keep commits scoped and include the tests for the changed behavior. Do not mix
unrelated upstream formatting, generated metadata, or user changes into a
commit. Check `git diff --check`, `git diff --stat`, and `git status --short`
before committing.

### Never Commit

Do not use `git add -f` to include any of these:

- `dist/`, extracted `rust-workbench.app` directories, archives, checksums, or
  generated release manifests.
- `.vm-cache/`, cloud images, QEMU overlays, cloud-init seeds, VM SSH keys, or
  VM serial logs.
- `rust/build/`, `rust/target/`, `zed-rust/target/`, analyzer `target/`
  directories, incremental state, or compiled binaries and libraries.
- `zed-rust/.build-deps/`, generated native sysroots, downloaded SDK packages,
  or build receipts.
- `zed-rust/rust-workbench-data/`, analysis targets, databases, caches,
  extensions, logs, hang traces, minidumps, or collected diagnostic archives.
- `rustc-ice-*.txt`, ad hoc reproduction binaries, temporary screenshots, or
  unredacted performance/debug output.
- `.agents/`, `.codex/`, editor-local state, credentials, tokens, SSH material,
  or machine-specific paths and settings.
- Local installations under `~/.local/opt`, `~/.local/bin`, desktop entries, or
  application data. Fix the source template and rebuild instead.

Some ignored files also exist in the imported upstream trees. Treat ignored
status as a boundary, not as an invitation to force-add a file. Update
`.gitignore` deliberately if a newly generated path is discovered.

### Publish Outside Git History

Build artifacts belong on the GitHub Release matching their annotated tag:

```text
rust-workbench-VERSION-linux-x86_64-glibc2.43.tar.zst
rust-workbench-VERSION-linux-x86_64-glibc2.43.tar.zst.sha256
rust-workbench-VERSION-linux-x86_64-glibc2.43.tar.zst.manifest.json
```

Upload those three files from `dist/` to the private repository's
`rust-workbench-vVERSION` release. Do not commit them. The manifest's
`source_commit` must equal the tagged commit, and the checksum must validate
before upload. Optional diagnostic archives are private support attachments,
not release assets or repository files.

There is intentionally no project CI/CD. Do not add a root workflow or turn the
inherited upstream workflow files into this project's release automation.
Local qualification is the release gate.

## Correctness Boundaries

- Treat custom LSP payloads and compiler output as untrusted input. Validate
  schema, status, IDs, ranges, text sizes, collection sizes, and graph sizes at
  the project bridge.
- Tie asynchronous results to the active source hash, selected issue,
  selection generation, and analyzer generation. Discard stale callbacks.
- Retain a prior diagram during a transport restart only when it still matches
  the active buffer and issue.
- Bound issue rendering, stderr capture, diagrams, diagnostics, retries, and
  cleanup. Large or malformed inputs must not create unbounded GPUI trees or
  memory growth.
- Avoid panic-prone indexing, `unwrap`, disconnected-channel assumptions, and
  duplicate completion assumptions on compiler/analyzer/UI boundaries.
- Repairs remain suggestions until compiler validation succeeds. Preserve the
  original source when validation fails.
- Preserve per-instance profile, database, log, cache, Cargo target, ownership
  artifact, and repair-target isolation.
- Do not weaken performance thresholds or rewrite the baseline merely to make a
  loaded-machine run pass. Use the CPU-idle preflight and investigate repeatable
  regressions.

## Most Used Commands

Run commands from the repository root unless the command says otherwise.

```bash
# Inspect prerequisites, source state, built artifacts, and ABI compatibility.
./workbench prerequisites build
./workbench prerequisites build --format apt
./workbench doctor

# Download and extract workspace-local native dependencies.
./workbench bootstrap

# Build everything, then run the doctor.
scripts/build-all

# Rebuild one component.
./workbench build compiler
./workbench build analyzer
./workbench build editor

# Open the main stress workspace.
scripts/run-workbench zed-rust/rust-ownership-stress-lab

# Open isolated additional instances.
scripts/run-workbench --new-instance zed-rust/rust-ownership-stress-lab
scripts/run-workbench --instance teaching-demo path/to/project
```

Use the narrowest useful test while iterating:

```bash
./workbench test resilience
./workbench test ui
./workbench test multi-instance
./workbench test performance
./workbench test quick
./workbench test full
python3 -m unittest discover -s tools/tests -p 'test_*.py'
```

- Analyzer, protocol, or compiler-transport changes: run `resilience` and
  `quick`; run `full` before publishing.
- Learning panel or diagram changes: run `ui` and `resilience`, inspect the
  actual app at desktop and mobile-width panel sizes, then run `quick`.
- Launcher, profile, or target-directory changes: run `multi-instance` and
  `resilience`.
- Performance-sensitive changes: run `performance` on an idle host.
- Compiler or cross-layer contract changes: run `full`.
- Work inside `zed-rust/` must also follow `zed-rust/.rules`; use
  `cd zed-rust && ./script/clippy` for its Clippy gate.

Useful maintenance commands:

```bash
./workbench disk
./workbench clean --dry-run
./workbench clean --debug-caches
./workbench diagnostics collect
./workbench diagnostics prune --dry-run
```

Do not run `./workbench clean --all`, include minidumps, or prune diagnostics
without confirming that the requested evidence and build outputs are no longer
needed.

## Release Procedure

1. Update the package version in
   `zed-rust/crates/rust_workbench/Cargo.toml`, its lockfile entry, and versioned
   documentation references together. The archive name is generated from that
   version and `workbench.toml` compatibility metadata.
2. Run the relevant focused gates, followed by `./workbench test full`.
3. Commit the intended source and confirm `git status --short` is empty.
4. Run `scripts/build-release`. Release packaging intentionally refuses a dirty
   worktree.
5. Confirm the mandatory Ubuntu 26.04 KVM gate within `scripts/build-release`
   passed. `scripts/verify-release --vm` repeats it for an existing archive;
   `--container` is an optional additional check.
6. Confirm the archive checksum and verify that both the sidecar and embedded
   manifests identify `git rev-parse HEAD`.
7. Create the annotated `rust-workbench-vVERSION` tag on that exact commit and
   push the source branch and tag to `origin`.
8. Create or update the matching private GitHub Release and upload only the
   archive, `.sha256`, and `.manifest.json` files.
9. Verify remote branch/tag commit identity and the published asset names and
   sizes. Install from the published archive and run its `--doctor` through both
   the desktop target and the user command.

The release target is Ubuntu 26.04, Linux x86-64, with glibc 2.43 or newer. The
host supplies Vulkan and X11 or Wayland. See `docs/BUILDING.md`,
`docs/PACKAGING.md`, and `docs/TROUBLESHOOTING.md` for operational details.
Do not tag, upload, replace a release asset, or install the replacement on the
host until the disposable VM gate passes. Moving an existing release tag is an
exceptional destructive operation and still requires explicit owner approval.
