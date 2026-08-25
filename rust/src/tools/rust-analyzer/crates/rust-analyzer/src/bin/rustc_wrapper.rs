//! We setup RUSTC_WRAPPER to point to `rust-analyzer` binary itself during the
//! initial `cargo check`. That way, we avoid checking the actual project, and
//! only build proc macros and build.rs.
//!
//! Code taken from IntelliJ :0)
//!     https://github.com/intellij-rust/intellij-rust/blob/master/native-helper/src/main.rs
use std::{
    ffi::OsString,
    fs, io,
    path::PathBuf,
    process::{Command, ExitCode, Stdio},
    time::SystemTime,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnershipArtifactStamp {
    path: PathBuf,
    length: u64,
    modified: Option<SystemTime>,
}

pub(crate) fn main() -> io::Result<ExitCode> {
    let mut args = std::env::args_os();
    let _me = args.next().unwrap();
    let rustc = args.next().unwrap();
    let args = args.collect();
    match std::env::var("RA_RUSTC_WRAPPER").as_deref() {
        Ok("borrowck-wrapper-suggestions") => run_rustc_with_wrapper_suggestions(rustc, args),
        Ok("ownership") => run_rustc_with_ownership(rustc, args),
        Ok("ownership-validation") => run_rustc_with_ownership_validation(rustc, args),
        _ => run_rustc_skipping_cargo_checking(rustc, args),
    }
}

fn run_rustc_with_ownership_validation(
    rustc_executable: OsString,
    args: Vec<OsString>,
) -> io::Result<ExitCode> {
    let original = std::env::var_os("RA_ORIGINAL_RUSTC_WORKSPACE_WRAPPER");
    let model_directory = std::env::var_os("RA_OWNERSHIP_MODEL_DIR");
    let (executable, args) =
        ownership_validation_invocation(rustc_executable, args, original, model_directory);
    run_rustc(executable, args)
}

fn run_rustc_with_ownership(
    rustc_executable: OsString,
    args: Vec<OsString>,
) -> io::Result<ExitCode> {
    let original = std::env::var_os("RA_ORIGINAL_RUSTC_WORKSPACE_WRAPPER");
    let model_directory = std::env::var_os("RA_OWNERSHIP_MODEL_DIR");
    let checks_metadata = checks_metadata(&args);
    let artifact_prefix = crate_name(&args).map(|name| format!("{name}-"));
    let artifacts_before =
        ownership_artifact_snapshot(model_directory.as_ref(), artifact_prefix.as_deref());
    let (executable, instrumented_args) = ownership_invocation(
        rustc_executable.clone(),
        args.clone(),
        original.clone(),
        model_directory.clone(),
    );
    let instrumented_status = run_rustc(executable, instrumented_args)?;

    // Some front-end errors stop the instrumented compiler before borrow checking. Run an
    // unmodified diagnostic pass only when no model was produced, so rust-analyzer still receives
    // rustc's complete error list without doubling the normal ownership-analysis workload.
    let artifacts_after =
        ownership_artifact_snapshot(model_directory.as_ref(), artifact_prefix.as_deref());
    if needs_diagnostic_fallback(
        checks_metadata,
        model_directory.is_some(),
        &artifacts_before,
        &artifacts_after,
    ) {
        let (executable, args) = forwarded_rustc_invocation(rustc_executable, args, original);
        return run_rustc(executable, args);
    }

    Ok(instrumented_status)
}

fn checks_metadata(args: &[OsString]) -> bool {
    args.iter().any(|arg| {
        let arg = arg.to_string_lossy();
        arg.starts_with("--emit=") && arg.contains("metadata") && !arg.contains("link")
    })
}

fn crate_name(args: &[OsString]) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == "--crate-name")
        .map(|pair| pair[1].to_string_lossy().into_owned())
}

fn ownership_artifact_snapshot(
    model_directory: Option<&OsString>,
    file_prefix: Option<&str>,
) -> Vec<OwnershipArtifactStamp> {
    let Some(model_directory) = model_directory else { return Vec::new() };
    let Ok(entries) = fs::read_dir(model_directory) else { return Vec::new() };
    let mut snapshot = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "json"))
        .filter(|entry| {
            file_prefix.is_none_or(|prefix| entry.file_name().to_string_lossy().starts_with(prefix))
        })
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            Some(OwnershipArtifactStamp {
                path: entry.path(),
                length: metadata.len(),
                modified: metadata.modified().ok(),
            })
        })
        .collect::<Vec<_>>();
    snapshot.sort_by(|left, right| left.path.cmp(&right.path));
    snapshot
}

fn needs_diagnostic_fallback(
    checks_metadata: bool,
    has_model_directory: bool,
    artifacts_before: &[OwnershipArtifactStamp],
    artifacts_after: &[OwnershipArtifactStamp],
) -> bool {
    checks_metadata && has_model_directory && artifacts_before == artifacts_after
}

fn ownership_invocation(
    rustc_executable: OsString,
    args: Vec<OsString>,
    original_wrapper: Option<OsString>,
    model_directory: Option<OsString>,
) -> (OsString, Vec<OsString>) {
    let (executable, mut args) =
        forwarded_rustc_invocation(rustc_executable, args, original_wrapper);
    if checks_metadata(&args) {
        match model_directory {
            Some(directory) => {
                let mut option = OsString::from("-Zborrowck-ownership-model=");
                option.push(directory);
                if !args
                    .iter()
                    .any(|arg| arg.to_string_lossy().starts_with("-Zborrowck-ownership-model="))
                {
                    args.push(option);
                }
            }
            None if !args.iter().any(|arg| arg == "-Zborrowck-ownership-events") => {
                args.push("-Zborrowck-ownership-events".into());
            }
            None => (),
        }
    }
    (executable, args)
}

fn ownership_validation_invocation(
    rustc_executable: OsString,
    args: Vec<OsString>,
    original_wrapper: Option<OsString>,
    model_directory: Option<OsString>,
) -> (OsString, Vec<OsString>) {
    let (executable, mut args) =
        ownership_wrapper_invocation(rustc_executable, args, original_wrapper);
    if checks_metadata(&args)
        && let Some(directory) = model_directory
        && !args.iter().any(|arg| arg.to_string_lossy().starts_with("-Zborrowck-ownership-model="))
    {
        let mut option = OsString::from("-Zborrowck-ownership-model=");
        option.push(directory);
        args.push(option);
    }
    (executable, args)
}

fn run_rustc_with_wrapper_suggestions(
    rustc_executable: OsString,
    args: Vec<OsString>,
) -> io::Result<ExitCode> {
    let original = std::env::var_os("RA_ORIGINAL_RUSTC_WORKSPACE_WRAPPER");
    let (executable, args) = ownership_wrapper_invocation(rustc_executable, args, original);
    run_rustc(executable, args)
}

fn ownership_wrapper_invocation(
    rustc_executable: OsString,
    mut args: Vec<OsString>,
    original_wrapper: Option<OsString>,
) -> (OsString, Vec<OsString>) {
    if checks_metadata(&args) && !args.iter().any(|arg| arg == "-Zborrowck-wrapper-suggestions") {
        args.push("-Zborrowck-wrapper-suggestions".into());
    }
    forwarded_rustc_invocation(rustc_executable, args, original_wrapper)
}

fn forwarded_rustc_invocation(
    rustc_executable: OsString,
    args: Vec<OsString>,
    original_wrapper: Option<OsString>,
) -> (OsString, Vec<OsString>) {
    match original_wrapper {
        Some(wrapper) => {
            let mut forwarded = Vec::with_capacity(args.len() + 1);
            forwarded.push(rustc_executable);
            forwarded.extend(args);
            (wrapper, forwarded)
        }
        None => (rustc_executable, args),
    }
}

fn run_rustc_skipping_cargo_checking(
    rustc_executable: OsString,
    args: Vec<OsString>,
) -> io::Result<ExitCode> {
    // `CARGO_CFG_TARGET_ARCH` is only set by cargo when executing build scripts
    // We don't want to exit out checks unconditionally with success if a build
    // script tries to invoke checks themselves
    // See https://github.com/rust-lang/rust-analyzer/issues/12973 for context
    let not_invoked_by_build_script = std::env::var_os("CARGO_CFG_TARGET_ARCH").is_none();
    let is_cargo_check = args.iter().any(|arg| {
        let arg = arg.to_string_lossy();
        // `cargo check` invokes `rustc` with `--emit=metadata` argument.
        //
        // https://doc.rust-lang.org/rustc/command-line-arguments.html#--emit-specifies-the-types-of-output-files-to-generate
        // link —     Generates the crates specified by --crate-type. The default
        //            output filenames depend on the crate type and platform. This
        //            is the default if --emit is not specified.
        // metadata — Generates a file containing metadata about the crate.
        //            The default output filename is CRATE_NAME.rmeta.
        arg.starts_with("--emit=") && arg.contains("metadata") && !arg.contains("link")
    });
    if not_invoked_by_build_script && is_cargo_check {
        Ok(ExitCode::from(0))
    } else {
        run_rustc(rustc_executable, args)
    }
}

fn run_rustc(rustc_executable: OsString, args: Vec<OsString>) -> io::Result<ExitCode> {
    #[allow(clippy::disallowed_methods)]
    let mut child = Command::new(rustc_executable)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    Ok(ExitCode::from(child.wait()?.code().unwrap_or(102) as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter().map(|arg| arg.into_string().unwrap()).collect()
    }

    #[test]
    fn appends_suggestion_flag_to_metadata_checks() {
        let (executable, args) = ownership_wrapper_invocation(
            "rustc".into(),
            vec!["--crate-name".into(), "app".into(), "--emit=dep-info,metadata".into()],
            None,
        );
        assert_eq!(executable, OsString::from("rustc"));
        assert_eq!(
            strings(args),
            ["--crate-name", "app", "--emit=dep-info,metadata", "-Zborrowck-wrapper-suggestions"]
        );
    }

    #[test]
    fn ownership_mode_appends_only_the_fast_model_flag() {
        let (executable, args) = ownership_invocation(
            "rustc".into(),
            vec!["--crate-name".into(), "app".into(), "--emit=metadata".into()],
            None,
            Some("/tmp/ownership".into()),
        );
        assert_eq!(executable, OsString::from("rustc"));
        assert_eq!(
            strings(args),
            ["--crate-name", "app", "--emit=metadata", "-Zborrowck-ownership-model=/tmp/ownership",]
        );
    }

    #[test]
    fn validation_mode_appends_model_and_suggestion_flags() {
        let (executable, args) = ownership_validation_invocation(
            "rustc".into(),
            vec!["--crate-name".into(), "app".into(), "--emit=metadata".into()],
            None,
            Some("/tmp/ownership".into()),
        );
        assert_eq!(executable, OsString::from("rustc"));
        assert_eq!(
            strings(args),
            [
                "--crate-name",
                "app",
                "--emit=metadata",
                "-Zborrowck-wrapper-suggestions",
                "-Zborrowck-ownership-model=/tmp/ownership",
            ]
        );
    }

    #[test]
    fn ownership_mode_falls_back_to_legacy_events_without_model_directory() {
        let (_, args) =
            ownership_invocation("rustc".into(), vec!["--emit=metadata".into()], None, None);
        assert_eq!(strings(args), ["--emit=metadata", "-Zborrowck-ownership-events"]);
    }

    #[test]
    fn preserves_original_workspace_wrapper() {
        let (executable, args) = ownership_wrapper_invocation(
            "rustc".into(),
            vec!["--emit=metadata".into()],
            Some("sccache".into()),
        );
        assert_eq!(executable, OsString::from("sccache"));
        assert_eq!(strings(args), ["rustc", "--emit=metadata", "-Zborrowck-wrapper-suggestions"]);
    }

    #[test]
    fn does_not_modify_linking_invocations() {
        let (_, args) =
            ownership_wrapper_invocation("rustc".into(), vec!["--emit=dep-info,link".into()], None);
        assert_eq!(strings(args), ["--emit=dep-info,link"]);
    }

    #[test]
    fn falls_back_to_complete_diagnostics_only_without_a_new_model() {
        let before = vec![OwnershipArtifactStamp {
            path: "/tmp/model.json".into(),
            length: 10,
            modified: None,
        }];
        let mut after = before.clone();

        assert!(needs_diagnostic_fallback(true, true, &before, &after));
        assert!(!needs_diagnostic_fallback(false, true, &before, &after));
        assert!(!needs_diagnostic_fallback(true, false, &before, &after));

        after[0].length += 1;
        assert!(!needs_diagnostic_fallback(true, true, &before, &after));
    }
}
