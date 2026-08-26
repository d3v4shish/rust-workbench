# Licensing

This monorepo combines upstream projects with different licenses. There is no
single root license that replaces their terms.

- The Rust source tree is dual-licensed under Apache-2.0 and MIT. See
  `rust/LICENSE-APACHE`, `rust/LICENSE-MIT`, and `rust/COPYRIGHT`.
- The Zed source tree contains GPL-3.0-or-later and Apache-2.0 licensed
  components. See `zed-rust/LICENSE-GPL`, `zed-rust/LICENSE-APACHE`, and the
  license metadata in individual crates.
- Third-party Rust crates and bundled native packages retain their respective
  licenses and notices.

The packaging code copies the Rust and Zed license texts into
`rust-workbench.app/share/licenses/rust-workbench` and includes available Cargo
documentation notices. Before redistributing a release, review the full bundled
dependency inventory and ensure every required third-party notice is present.
