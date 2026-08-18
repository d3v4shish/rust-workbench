use run_make_support::{cargo, path, rfs};

fn main() {
    let source = path("src/main.rs");
    let original = rfs::read_to_string(&source);

    let output = cargo()
        .args(["check", "--message-format=json"])
        .env("RUSTFLAGS", "-Zborrowck-wrapper-suggestions")
        .env("CARGO_TARGET_DIR", path("target"))
        .run_fail();

    let stdout = output.stdout_utf8();
    assert!(stdout.contains("\"code\":\"borrowck_wrapper_ref_cell\""), "{stdout}");
    assert!(
        stdout.contains("Use RefCell for interior mutability (compiler validated)"),
        "{stdout}"
    );
    assert!(stdout.contains("\"suggestion_applicability\":\"MaybeIncorrect\""), "{stdout}");
    assert_eq!(rfs::read_to_string(source), original);
}
