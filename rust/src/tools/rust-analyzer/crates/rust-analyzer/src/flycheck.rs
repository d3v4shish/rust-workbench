//! Flycheck provides the functionality needed to run `cargo check` to provide
//! LSP diagnostics based on the output of the command.

use std::{
    cell::Cell,
    ffi::OsString,
    fmt, io,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, SystemTime},
};

use cargo_metadata::PackageId;
use crossbeam_channel::{Receiver, Sender, select_biased, unbounded};
use ide_db::FxHashSet;
use itertools::Itertools;
use paths::{AbsPath, AbsPathBuf, Utf8Path, Utf8PathBuf};
use project_model::TargetDirectoryConfig;
use project_model::project_json;
use rustc_hash::FxHashMap;
use serde::Deserialize as _;
use serde_derive::Deserialize;

pub(crate) use cargo_metadata::diagnostic::{
    Applicability, Diagnostic, DiagnosticCode, DiagnosticLevel, DiagnosticSpan,
};
use toolchain::Tool;
use triomphe::Arc;

use crate::{
    command::{CommandHandle, JsonLinesParser},
    diagnostics::DiagnosticsGeneration,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum InvocationStrategy {
    Once,
    #[default]
    PerWorkspace,
}

/// Data needed to construct a `cargo` command invocation, e.g. for flycheck or running a test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CargoOptions {
    /// The cargo subcommand to run, e.g. "check" or "clippy"
    pub(crate) subcommand: String,
    pub(crate) target_tuples: Vec<String>,
    pub(crate) all_targets: bool,
    pub(crate) set_test: bool,
    pub(crate) no_default_features: bool,
    pub(crate) all_features: bool,
    pub(crate) features: Vec<String>,
    pub(crate) extra_args: Vec<String>,
    pub(crate) extra_test_bin_args: Vec<String>,
    pub(crate) extra_env: FxHashMap<String, Option<String>>,
    pub(crate) config_path: Option<AbsPathBuf>,
    pub(crate) target_dir_config: TargetDirectoryConfig,
}

#[derive(Clone, Debug)]
pub(crate) enum Target {
    Bin(String),
    Example(String),
    Benchmark(String),
    Test(String),
}

impl CargoOptions {
    pub(crate) fn apply_on_command(
        &self,
        cmd: &mut Command,
        ws_target_dir: Option<&Utf8Path>,
        package_repr: Option<&str>,
        toolchain_version: Option<&semver::Version>,
    ) {
        toolchain::cargo_use_targets(toolchain_version, cmd, &self.target_tuples);
        if self.all_targets {
            if self.set_test {
                cmd.arg("--all-targets");
            } else {
                // No --benches unfortunately, as this implies --tests (see https://github.com/rust-lang/cargo/issues/6454),
                // and users setting `cfg.seTest = false` probably prefer disabling benches than enabling tests.
                cmd.args(["--lib", "--bins", "--examples"]);
            }
        }
        if self.all_features {
            cmd.arg("--all-features");
        } else {
            if self.no_default_features {
                cmd.arg("--no-default-features");
            }
            if !self.features.is_empty() {
                // If we are scoped to a particular package, filter any features of the form
                // `crate/feature` which target other packages.
                let features = if let Some(name) = package_repr {
                    let filtered = self
                        .features
                        .iter()
                        .filter(|f| match f.split_once('/') {
                            Some((c, _)) => c == name,
                            None => true,
                        })
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>();
                    filtered.join(" ")
                } else {
                    self.features.join(" ")
                };
                cmd.arg("--features");
                cmd.arg(features);
            }
        }
        if let Some(config_path) = &self.config_path {
            cmd.arg("--config").arg(config_path);
        }
        if let Some(target_dir) = self.target_dir_config.target_dir(ws_target_dir) {
            cmd.arg("--target-dir").arg(target_dir.as_ref());
        }
    }
}

/// The flycheck config from a rust-project.json file or discoverConfig JSON output.
#[derive(Debug, Default)]
pub(crate) struct FlycheckConfigJson {
    /// The template with [project_json::RunnableKind::Flycheck]
    pub single_template: Option<project_json::Runnable>,
}

impl FlycheckConfigJson {
    pub(crate) fn any_configured(&self) -> bool {
        // self.workspace_template.is_some() ||
        self.single_template.is_some()
    }
}

/// The flycheck config from rust-analyzer's own configuration.
///
/// We rely on this when rust-project.json does not specify a flycheck runnable
///
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FlycheckConfig {
    /// Automatically use rust-project.json's flycheck runnable or just use cargo (the common case)
    ///
    /// We can't have a variant for ProjectJson because that is configured on the fly during
    /// discoverConfig. We only know what we can read at config time.
    Automatic {
        /// If we do use cargo, how to build the check command
        cargo_options: CargoOptions,
        ansi_color_output: bool,
        ownership_visualization: bool,
        ownership_wrapper_suggestions: bool,
    },
    /// check_overrideCommand. This overrides both cargo and rust-project.json's flycheck runnable.
    CustomCommand {
        command: String,
        args: Vec<String>,
        extra_env: FxHashMap<String, Option<String>>,
        invocation_strategy: InvocationStrategy,
    },
}

impl FlycheckConfig {
    pub(crate) fn invocation_strategy(&self) -> InvocationStrategy {
        match self {
            FlycheckConfig::Automatic { .. } => InvocationStrategy::PerWorkspace,
            FlycheckConfig::CustomCommand { invocation_strategy, .. } => {
                invocation_strategy.clone()
            }
        }
    }
}

impl fmt::Display for FlycheckConfig {
    /// Show a shortened version of the check command.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlycheckConfig::Automatic { cargo_options, .. } => {
                write!(f, "cargo {}", cargo_options.subcommand)
            }
            FlycheckConfig::CustomCommand { command, args, .. } => {
                // Don't show `my_custom_check --foo $saved_file` literally to the user, as it
                // looks like we've forgotten to substitute $saved_file.
                //
                // `my_custom_check --foo /home/user/project/src/dir/foo.rs` is too verbose.
                //
                // Instead, show `my_custom_check --foo ...`. The
                // actual path is often too long to be worth showing
                // in the IDE (e.g. in the VS Code status bar).
                let display_args = args
                    .iter()
                    .map(|arg| {
                        if (arg == SAVED_FILE_PLACEHOLDER_DOLLAR)
                            || (arg == SAVED_FILE_INLINE)
                            || arg.ends_with(".rs")
                        {
                            "..."
                        } else {
                            arg
                        }
                    })
                    .collect::<Vec<_>>();

                write!(f, "{command} {}", display_args.join(" "))
            }
        }
    }
}

/// Flycheck wraps the shared state and communication machinery used for
/// running `cargo check` (or other compatible command) and providing
/// diagnostics based on the output.
/// The spawned thread is shut down when this struct is dropped.
#[derive(Debug)]
pub(crate) struct FlycheckHandle {
    // XXX: drop order is significant
    sender: Sender<StateChange>,
    _thread: stdx::thread::JoinHandle,
    id: usize,
    generation: Arc<AtomicUsize>,
}

impl FlycheckHandle {
    pub(crate) fn spawn(
        id: usize,
        generation: Arc<AtomicUsize>,
        sender: Sender<FlycheckMessage>,
        config: FlycheckConfig,
        config_json: FlycheckConfigJson,
        sysroot_root: Option<AbsPathBuf>,
        workspace_root: AbsPathBuf,
        manifest_path: Option<AbsPathBuf>,
        ws_target_dir: Option<Utf8PathBuf>,
        toolchain_version: Option<semver::Version>,
    ) -> FlycheckHandle {
        let actor = FlycheckActor::new(
            id,
            generation.load(Ordering::Relaxed),
            sender,
            config,
            config_json,
            sysroot_root,
            workspace_root,
            manifest_path,
            ws_target_dir,
            toolchain_version,
        );
        let (sender, receiver) = unbounded::<StateChange>();
        let thread =
            stdx::thread::Builder::new(stdx::thread::ThreadIntent::Worker, format!("Flycheck{id}"))
                .spawn(move || actor.run(receiver))
                .expect("failed to spawn thread");
        FlycheckHandle { id, generation, sender, _thread: thread }
    }

    /// Schedule a re-start of the cargo check worker to do a workspace wide check.
    pub(crate) fn restart_workspace(&self, saved_file: Option<AbsPathBuf>) {
        self.restart_workspace_with_validation(saved_file, false);
    }

    pub(crate) fn restart_workspace_with_validation(
        &self,
        saved_file: Option<AbsPathBuf>,
        validate_ownership_repairs: bool,
    ) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.sender
            .send(StateChange::Restart {
                generation,
                scope: FlycheckScope::Workspace,
                saved_file,
                target: None,
                validate_ownership_repairs,
            })
            .unwrap();
    }

    pub(crate) fn restart_for_package_with_validation(
        &self,
        package: PackageSpecifier,
        target: Option<Target>,
        workspace_deps: Option<FxHashSet<PackageSpecifier>>,
        saved_file: Option<AbsPathBuf>,
        validate_ownership_repairs: bool,
    ) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.sender
            .send(StateChange::Restart {
                generation,
                scope: FlycheckScope::Package { package, workspace_deps },
                saved_file,
                target,
                validate_ownership_repairs,
            })
            .unwrap();
    }

    /// Stop this cargo check worker.
    pub(crate) fn cancel(&self) {
        self.sender.send(StateChange::Cancel).unwrap();
    }

    pub(crate) fn id(&self) -> usize {
        self.id
    }

    pub(crate) fn generation(&self) -> DiagnosticsGeneration {
        self.generation.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
pub(crate) enum ClearDiagnosticsKind {
    All(ClearScope),
    OlderThan(DiagnosticsGeneration, ClearScope),
}

#[derive(Debug)]
pub(crate) enum ClearScope {
    Workspace,
    Package(PackageSpecifier),
}

pub(crate) enum FlycheckMessage {
    /// Request adding a diagnostic with fixes included to a file
    AddDiagnostic {
        id: usize,
        generation: DiagnosticsGeneration,
        workspace_root: Arc<AbsPathBuf>,
        diagnostic: Diagnostic,
        package_id: Option<PackageSpecifier>,
    },

    /// Request clearing all outdated diagnostics.
    ClearDiagnostics { id: usize, kind: ClearDiagnosticsKind },

    /// Request loading an ownership artifact retained for this workspace. Cargo can report a
    /// successful target as fresh without invoking rustc again, so the artifact transport must
    /// not depend solely on diagnostics from the current command.
    OwnershipArtifacts { id: usize, paths: Vec<std::path::PathBuf> },

    /// Request check progress notification to client
    Progress {
        /// Flycheck instance ID
        id: usize,
        progress: Progress,
    },
}

impl fmt::Debug for FlycheckMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlycheckMessage::AddDiagnostic {
                id,
                generation,
                workspace_root,
                diagnostic,
                package_id,
            } => f
                .debug_struct("AddDiagnostic")
                .field("id", id)
                .field("generation", generation)
                .field("workspace_root", workspace_root)
                .field("package_id", package_id)
                .field("diagnostic_code", &diagnostic.code.as_ref().map(|it| &it.code))
                .finish(),
            FlycheckMessage::ClearDiagnostics { id, kind } => {
                f.debug_struct("ClearDiagnostics").field("id", id).field("kind", kind).finish()
            }
            FlycheckMessage::OwnershipArtifacts { id, paths } => f
                .debug_struct("OwnershipArtifacts")
                .field("id", id)
                .field("count", &paths.len())
                .finish(),
            FlycheckMessage::Progress { id, progress } => {
                f.debug_struct("Progress").field("id", id).field("progress", progress).finish()
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum Progress {
    DidStart {
        /// The user sees this in VSCode, etc. May be a shortened version of the command we actually
        /// executed, otherwise it is way too long.
        user_facing_command: String,
    },
    DidCheckCrate(String),
    DidFinish(io::Result<()>),
    DidCancel,
    DidFailToRestart(String),
}

#[derive(Debug, Clone)]
enum FlycheckScope {
    Workspace,
    Package {
        // Either a cargo package or a $label in rust-project.check.overrideCommand
        package: PackageSpecifier,
        workspace_deps: Option<FxHashSet<PackageSpecifier>>,
    },
}

#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub(crate) enum PackageSpecifier {
    Cargo {
        /// The one in Cargo.toml, assumed to work with `cargo check -p {}` etc
        package_id: Arc<PackageId>,
    },
    BuildInfo {
        /// If a `build` field is present in rust-project.json, its label field
        label: String,
    },
}

impl PackageSpecifier {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Cargo { package_id } => &package_id.repr,
            Self::BuildInfo { label } => label,
        }
    }
}

#[derive(Debug)]
enum FlycheckCommandOrigin {
    /// Regular cargo invocation
    Cargo,
    /// Configured via check_overrideCommand
    CheckOverrideCommand,
    /// From a runnable with [project_json::RunnableKind::Flycheck]
    ProjectJsonRunnable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnershipWrapperMode {
    Model,
    Suggestions,
    ModelAndSuggestions,
}

#[derive(Debug)]
enum StateChange {
    Restart {
        generation: DiagnosticsGeneration,
        scope: FlycheckScope,
        saved_file: Option<AbsPathBuf>,
        target: Option<Target>,
        validate_ownership_repairs: bool,
    },
    Cancel,
}

/// A [`FlycheckActor`] is a single check instance of a workspace.
struct FlycheckActor {
    /// The workspace id of this flycheck instance.
    id: usize,

    generation: DiagnosticsGeneration,
    sender: Sender<FlycheckMessage>,
    config: FlycheckConfig,
    config_json: FlycheckConfigJson,

    manifest_path: Option<AbsPathBuf>,
    ws_target_dir: Option<Utf8PathBuf>,
    /// Either the workspace root of the workspace we are flychecking,
    /// or the project root of the project.
    root: Arc<AbsPathBuf>,
    sysroot_root: Option<AbsPathBuf>,
    scope: FlycheckScope,
    /// CargoHandle exists to wrap around the communication needed to be able to
    /// run `cargo check` without blocking. Currently the Rust standard library
    /// doesn't provide a way to read sub-process output without blocking, so we
    /// have to wrap sub-processes output handling in a thread and pass messages
    /// back over a channel.
    command_handle: Option<CommandHandle<CheckMessage>>,
    /// The receiver side of the channel mentioned above.
    command_receiver: Option<Receiver<CheckMessage>>,
    diagnostics_cleared_for: FxHashSet<PackageSpecifier>,
    diagnostics_received: DiagnosticsReceived,
    toolchain_version: Option<semver::Version>,
    ownership_wrapper_support: Cell<Option<bool>>,
    warned_unsupported_ownership_wrapper: Cell<bool>,
}

#[derive(PartialEq, Debug)]
enum DiagnosticsReceived {
    /// We started a flycheck, but we haven't seen any diagnostics yet.
    NotYet,
    /// We received a non-zero number of diagnostics from rustc or clippy (via
    /// cargo or custom check command). This means there were errors or
    /// warnings.
    AtLeastOne,
    /// We received a non-zero number of diagnostics, and the scope is
    /// workspace, so we've discarded the previous workspace diagnostics.
    AtLeastOneAndClearedWorkspace,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum Event {
    RequestStateChange(StateChange),
    CheckEvent(Option<CheckMessage>),
}

/// This is stable behaviour. Don't change.
const SAVED_FILE_PLACEHOLDER_DOLLAR: &str = "$saved_file";
const LABEL_INLINE: &str = "{label}";
const SAVED_FILE_INLINE: &str = "{saved_file}";

#[derive(Debug)]
struct Substitutions<'a> {
    label: Option<&'a str>,
    saved_file: Option<&'a str>,
}

impl<'a> Substitutions<'a> {
    /// If you have a runnable, and it has {label} in it somewhere, treat it as a template that
    /// may be unsatisfied if you do not provide a label to substitute into it. Returns None in
    /// that situation. Otherwise performs the requested substitutions.
    ///
    /// Same for {saved_file}.
    ///
    #[allow(clippy::disallowed_types)] /* generic parameter allows for FxHashMap */
    fn substitute<H>(
        self,
        template: &project_json::Runnable,
        extra_env: &std::collections::HashMap<String, Option<String>, H>,
    ) -> Option<Command> {
        let mut cmd = toolchain::command(&template.program, &template.cwd, extra_env);
        for arg in &template.args {
            if let Some(ix) = arg.find(LABEL_INLINE) {
                let label = self.label?;
                let mut arg = arg.to_string();
                arg.replace_range(ix..ix + LABEL_INLINE.len(), label);
                cmd.arg(arg);
                continue;
            }
            if let Some(ix) = arg.find(SAVED_FILE_INLINE) {
                let saved_file = self.saved_file?;
                let mut arg = arg.to_string();
                arg.replace_range(ix..ix + SAVED_FILE_INLINE.len(), saved_file);
                cmd.arg(arg);
                continue;
            }
            // Legacy syntax: full argument match
            if arg == SAVED_FILE_PLACEHOLDER_DOLLAR {
                let saved_file = self.saved_file?;
                cmd.arg(saved_file);
                continue;
            }
            cmd.arg(arg);
        }
        cmd.current_dir(&template.cwd);
        Some(cmd)
    }
}

impl FlycheckActor {
    fn new(
        id: usize,
        generation: DiagnosticsGeneration,
        sender: Sender<FlycheckMessage>,
        config: FlycheckConfig,
        config_json: FlycheckConfigJson,
        sysroot_root: Option<AbsPathBuf>,
        workspace_root: AbsPathBuf,
        manifest_path: Option<AbsPathBuf>,
        ws_target_dir: Option<Utf8PathBuf>,
        toolchain_version: Option<semver::Version>,
    ) -> FlycheckActor {
        tracing::info!(%id, ?workspace_root, "Spawning flycheck");
        FlycheckActor {
            id,
            generation,
            sender,
            config,
            config_json,
            sysroot_root,
            root: Arc::new(workspace_root),
            scope: FlycheckScope::Workspace,
            manifest_path,
            ws_target_dir,
            command_handle: None,
            command_receiver: None,
            diagnostics_cleared_for: Default::default(),
            diagnostics_received: DiagnosticsReceived::NotYet,
            toolchain_version,
            ownership_wrapper_support: Cell::new(None),
            warned_unsupported_ownership_wrapper: Cell::new(false),
        }
    }

    fn report_progress(&self, progress: Progress) {
        self.send(FlycheckMessage::Progress { id: self.id, progress });
    }

    fn next_event(&self, inbox: &Receiver<StateChange>) -> Option<Event> {
        let Some(command_receiver) = &self.command_receiver else {
            return inbox.recv().ok().map(Event::RequestStateChange);
        };

        // Biased to give restarts a preference so check outputs don't block a restart or stop
        select_biased! {
            recv(inbox) -> msg => msg.ok().map(Event::RequestStateChange),
            recv(command_receiver) -> msg => Some(Event::CheckEvent(msg.ok())),
        }
    }

    fn run(mut self, inbox: Receiver<StateChange>) {
        'event: while let Some(event) = self.next_event(&inbox) {
            match event {
                Event::RequestStateChange(StateChange::Cancel) => {
                    tracing::debug!(flycheck_id = self.id, "flycheck cancelled");
                    self.cancel_check_process();
                }
                Event::RequestStateChange(StateChange::Restart {
                    mut generation,
                    mut scope,
                    mut saved_file,
                    mut target,
                    mut validate_ownership_repairs,
                }) => {
                    // Cancel the previously spawned process
                    self.cancel_check_process();

                    // Debounce by briefly waiting for other state changes.
                    while let Ok(restart) = inbox.recv_timeout(Duration::from_millis(50)) {
                        match restart {
                            StateChange::Cancel => {
                                // We got a cancel straight after this restart request, so
                                // don't do anything.
                                continue 'event;
                            }
                            StateChange::Restart {
                                generation: g,
                                scope: s,
                                saved_file: sf,
                                target: t,
                                validate_ownership_repairs: validate,
                            } => {
                                // We got another restart request. Take the parameters
                                // from the last restart request in this time window,
                                // because the most recent request is probably the most
                                // relevant to the user.
                                generation = g;
                                scope = s;
                                saved_file = sf;
                                target = t;
                                validate_ownership_repairs = validate;
                            }
                        }
                    }

                    let command = self.check_command(
                        &scope,
                        saved_file.as_deref(),
                        target,
                        validate_ownership_repairs,
                    );
                    self.scope = scope.clone();
                    self.generation = generation;

                    let Some((command, origin)) = command else {
                        tracing::debug!(?scope, "failed to build flycheck command");
                        continue;
                    };

                    let debug_command = format!("{command:?}");
                    let user_facing_command = self.config.to_string();

                    tracing::debug!(?origin, ?command, "will restart flycheck");
                    let (sender, receiver) = unbounded();
                    match CommandHandle::spawn(
                        command,
                        CheckParser,
                        sender,
                        match &self.config {
                            FlycheckConfig::Automatic { cargo_options, .. } => {
                                let ws_target_dir =
                                    self.ws_target_dir.as_ref().map(Utf8PathBuf::as_path);
                                let target_dir =
                                    cargo_options.target_dir_config.target_dir(ws_target_dir);

                                // If `"rust-analyzer.cargo.targetDir": null`, we should use
                                // workspace's target dir instead of hard-coded fallback.
                                let target_dir = target_dir.as_deref().or(ws_target_dir);

                                Some(
                                    // As `CommandHandle::spawn`'s working directory is
                                    // rust-analyzer's working directory, which might be different
                                    // from the flycheck's working directory, we should canonicalize
                                    // the output directory, otherwise we might write it into the
                                    // wrong target dir.
                                    // If `target_dir` is an absolute path, it will replace
                                    // `self.root` and that's an intended behavior.
                                    self.root
                                        .join(target_dir.unwrap_or(
                                            Utf8Path::new("target").join("rust-analyzer").as_path(),
                                        ))
                                        .join(format!("flycheck{}", self.id))
                                        .into(),
                                )
                            }
                            _ => None,
                        },
                    ) {
                        Ok(command_handle) => {
                            tracing::debug!(?origin, command = %debug_command, "did restart flycheck");
                            self.command_handle = Some(command_handle);
                            self.command_receiver = Some(receiver);
                            self.report_progress(Progress::DidStart { user_facing_command });
                        }
                        Err(error) => {
                            self.report_progress(Progress::DidFailToRestart(format!(
                                "Failed to run the following command: {debug_command} origin={origin:?} error={error}"
                            )));
                        }
                    }
                }
                Event::CheckEvent(None) => {
                    tracing::debug!(flycheck_id = self.id, "flycheck finished");

                    // Watcher finished
                    let command_handle = self.command_handle.take().unwrap();
                    self.command_receiver.take();
                    let formatted_handle = format!("{command_handle:?}");

                    let res = command_handle.join();
                    if let Err(error) = &res {
                        tracing::error!(
                            "Flycheck failed to run the following command: {}, error={}",
                            formatted_handle,
                            error
                        );
                    }
                    if self.diagnostics_received == DiagnosticsReceived::NotYet {
                        tracing::trace!(flycheck_id = self.id, "clearing diagnostics");
                        // We finished without receiving any diagnostics.
                        //
                        // `cargo check` generally outputs something, even if there are no
                        // warnings/errors, so we always know which package was checked.
                        //
                        // ```text
                        // $ cargo check --message-format=json 2>/dev/null
                        // {"reason":"compiler-artifact","package_id":"path+file:///Users/wilfred/tmp/scratch#0.1.0",...}
                        // ```
                        //
                        // However, rustc only returns JSON if there are diagnostics present, so a
                        // build without warnings or errors has an empty output.
                        //
                        // ```
                        // $ rustc --error-format=json bad.rs
                        // {"$message_type":"diagnostic","message":"mismatched types","...}
                        //
                        // $ rustc --error-format=json good.rs
                        // ```
                        //
                        // So if we got zero diagnostics, it was almost certainly a check that
                        // wasn't specific to a package.
                        self.send(FlycheckMessage::ClearDiagnostics {
                            id: self.id,
                            kind: ClearDiagnosticsKind::All(ClearScope::Workspace),
                        });
                    } else if res.is_ok() {
                        // We clear diagnostics for packages on
                        // `[CargoCheckMessage::CompilerArtifact]` but there seem to be setups where
                        // cargo may not report an artifact to our runner at all. To handle such
                        // cases, clear stale diagnostics when flycheck completes successfully.
                        match &self.scope {
                            FlycheckScope::Workspace => {
                                self.send(FlycheckMessage::ClearDiagnostics {
                                    id: self.id,
                                    kind: ClearDiagnosticsKind::OlderThan(
                                        self.generation,
                                        ClearScope::Workspace,
                                    ),
                                });
                            }
                            FlycheckScope::Package { package, workspace_deps } => {
                                for pkg in
                                    std::iter::once(package).chain(workspace_deps.iter().flatten())
                                {
                                    self.send(FlycheckMessage::ClearDiagnostics {
                                        id: self.id,
                                        kind: ClearDiagnosticsKind::OlderThan(
                                            self.generation,
                                            ClearScope::Package(pkg.clone()),
                                        ),
                                    });
                                }
                            }
                        }
                    }
                    self.clear_diagnostics_state();

                    self.report_cached_ownership_artifacts();

                    self.report_progress(Progress::DidFinish(res));
                }
                Event::CheckEvent(Some(message)) => match message {
                    CheckMessage::CompilerArtifact(msg) => {
                        tracing::trace!(
                            flycheck_id = self.id,
                            artifact = msg.target.name,
                            package_id = msg.package_id.repr,
                            "artifact received"
                        );
                        self.report_progress(Progress::DidCheckCrate(format!(
                            "{} ({})",
                            msg.target.name,
                            msg.target.kind.iter().format_with(", ", |kind, f| f(&kind)),
                        )));
                        let package_id = Arc::new(msg.package_id);
                        if self
                            .diagnostics_cleared_for
                            .insert(PackageSpecifier::Cargo { package_id: package_id.clone() })
                        {
                            tracing::trace!(
                                flycheck_id = self.id,
                                package_id = package_id.repr,
                                "clearing diagnostics"
                            );
                            self.send(FlycheckMessage::ClearDiagnostics {
                                id: self.id,
                                kind: ClearDiagnosticsKind::All(ClearScope::Package(
                                    PackageSpecifier::Cargo { package_id },
                                )),
                            });
                        }
                    }
                    CheckMessage::Diagnostic { diagnostic, package_id } => {
                        tracing::trace!(
                            flycheck_id = self.id,
                            message = diagnostic.message,
                            package_id = package_id.as_ref().map(|it| it.as_str()),
                            "diagnostic received"
                        );
                        if self.diagnostics_received == DiagnosticsReceived::NotYet {
                            self.diagnostics_received = DiagnosticsReceived::AtLeastOne;
                        }
                        if let Some(package_id) = &package_id {
                            if self.diagnostics_cleared_for.insert(package_id.clone()) {
                                tracing::trace!(
                                    flycheck_id = self.id,
                                    package_id = package_id.as_str(),
                                    "clearing diagnostics"
                                );
                                self.send(FlycheckMessage::ClearDiagnostics {
                                    id: self.id,
                                    kind: ClearDiagnosticsKind::All(ClearScope::Package(
                                        package_id.clone(),
                                    )),
                                });
                            }
                        } else if self.diagnostics_received
                            != DiagnosticsReceived::AtLeastOneAndClearedWorkspace
                        {
                            self.diagnostics_received =
                                DiagnosticsReceived::AtLeastOneAndClearedWorkspace;
                            self.send(FlycheckMessage::ClearDiagnostics {
                                id: self.id,
                                kind: ClearDiagnosticsKind::All(ClearScope::Workspace),
                            });
                        }
                        self.send(FlycheckMessage::AddDiagnostic {
                            id: self.id,
                            generation: self.generation,
                            package_id,
                            workspace_root: self.root.clone(),
                            diagnostic,
                        });
                    }
                },
            }
        }
        // If we rerun the thread, we need to discard the previous check results first
        self.cancel_check_process();
    }

    fn cancel_check_process(&mut self) {
        if let Some(command_handle) = self.command_handle.take() {
            tracing::debug!(
                command = ?command_handle,
                "did cancel flycheck"
            );
            command_handle.cancel();
            self.command_receiver.take();
            self.report_progress(Progress::DidCancel);
        }
        self.clear_diagnostics_state();
    }

    fn clear_diagnostics_state(&mut self) {
        self.diagnostics_cleared_for.clear();
        self.diagnostics_received = DiagnosticsReceived::NotYet;
    }

    fn explicit_check_command(
        &self,
        scope: &FlycheckScope,
        saved_file: Option<&AbsPath>,
    ) -> Option<Command> {
        let label = match scope {
            // We could add a runnable like "RunnableKind::FlycheckWorkspace". But generally
            // if you're not running cargo, it's because your workspace is too big to check
            // all at once. You can always use `check_overrideCommand` with no {label}.
            FlycheckScope::Workspace => return None,
            FlycheckScope::Package { package: PackageSpecifier::BuildInfo { label }, .. } => {
                label.as_str()
            }
            FlycheckScope::Package {
                package: PackageSpecifier::Cargo { package_id: label },
                ..
            } => &label.repr,
        };
        let template = self.config_json.single_template.as_ref()?;
        let subs = Substitutions { label: Some(label), saved_file: saved_file.map(|x| x.as_str()) };
        subs.substitute(template, &FxHashMap::default())
    }

    /// Construct a `Command` object for checking the user's code. If the user
    /// has specified a custom command with placeholders that we cannot fill,
    /// return None.
    fn check_command(
        &self,
        scope: &FlycheckScope,
        saved_file: Option<&AbsPath>,
        target: Option<Target>,
        validate_ownership_repairs: bool,
    ) -> Option<(Command, FlycheckCommandOrigin)> {
        match &self.config {
            FlycheckConfig::Automatic {
                cargo_options,
                ansi_color_output,
                ownership_visualization,
                ownership_wrapper_suggestions,
            } => {
                // Only use the rust-project.json's flycheck config when no check_overrideCommand
                // is configured. In the FlycheckConcig::CustomCommand branch we will still do
                // label substitution, but on the overrideCommand instead.
                //
                // There needs to be SOME way to override what your discoverConfig tool says,
                // because to change the flycheck runnable there you may have to literally
                // recompile the tool.
                if self.config_json.any_configured() {
                    // Completely handle according to rust-project.json.
                    // We don't consider this to be "using cargo" so we will not apply any of the
                    // CargoOptions to the command.
                    let cmd = self.explicit_check_command(scope, saved_file)?;
                    return Some((cmd, FlycheckCommandOrigin::ProjectJsonRunnable));
                }

                let mut cmd =
                    toolchain::command(Tool::Cargo.path(), &*self.root, &cargo_options.extra_env);
                if let Some(sysroot_root) = &self.sysroot_root
                    && !cargo_options.extra_env.contains_key("RUSTUP_TOOLCHAIN")
                    && std::env::var_os("RUSTUP_TOOLCHAIN").is_none()
                {
                    cmd.env("RUSTUP_TOOLCHAIN", AsRef::<std::path::Path>::as_ref(sysroot_root));
                }
                cmd.env("CARGO_LOG", "cargo::core::compiler::fingerprint=info");
                if *ownership_visualization && self.supports_ownership_events(cargo_options) {
                    self.configure_ownership_wrapper(
                        &mut cmd,
                        cargo_options,
                        if validate_ownership_repairs {
                            OwnershipWrapperMode::ModelAndSuggestions
                        } else {
                            OwnershipWrapperMode::Model
                        },
                    );
                } else if *ownership_wrapper_suggestions
                    && self.supports_ownership_wrapper(cargo_options)
                {
                    self.configure_ownership_wrapper(
                        &mut cmd,
                        cargo_options,
                        OwnershipWrapperMode::Suggestions,
                    );
                }
                cmd.arg(&cargo_options.subcommand);

                let package_repr = match scope {
                    FlycheckScope::Workspace => {
                        cmd.arg("--workspace");
                        None
                    }
                    FlycheckScope::Package {
                        package: PackageSpecifier::Cargo { package_id },
                        ..
                    } => {
                        cmd.arg("-p").arg(&package_id.repr);
                        Some(package_id.repr.as_str())
                    }
                    FlycheckScope::Package {
                        package: PackageSpecifier::BuildInfo { .. }, ..
                    } => {
                        // No way to flycheck this single package. All we have is a build label.
                        // There's no way to really say whether this build label happens to be
                        // a cargo canonical name, so we won't try.
                        return None;
                    }
                };

                if let Some(tgt) = target {
                    match tgt {
                        Target::Bin(tgt) => cmd.arg("--bin").arg(tgt),
                        Target::Example(tgt) => cmd.arg("--example").arg(tgt),
                        Target::Test(tgt) => cmd.arg("--test").arg(tgt),
                        Target::Benchmark(tgt) => cmd.arg("--bench").arg(tgt),
                    };
                }

                cmd.arg(if *ansi_color_output {
                    "--message-format=json-diagnostic-rendered-ansi"
                } else {
                    "--message-format=json"
                });

                if let Some(manifest_path) = &self.manifest_path {
                    cmd.arg("--manifest-path");
                    cmd.arg(manifest_path);
                    if manifest_path.extension() == Some("rs") {
                        cmd.env("__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS", "nightly");
                        cmd.arg("-Zscript");
                    }
                }

                cmd.arg("--keep-going");

                let mut validation_options;
                let effective_options = if validate_ownership_repairs {
                    validation_options = cargo_options.clone();
                    validation_options.target_dir_config =
                        TargetDirectoryConfig::Directory(ownership_validation_target_directory(
                            cargo_options,
                            self.ws_target_dir.as_ref().map(Utf8PathBuf::as_path),
                        ));
                    &validation_options
                } else {
                    cargo_options
                };
                effective_options.apply_on_command(
                    &mut cmd,
                    self.ws_target_dir.as_ref().map(Utf8PathBuf::as_path),
                    package_repr,
                    self.toolchain_version.as_ref(),
                );
                cmd.args(&cargo_options.extra_args);
                Some((cmd, FlycheckCommandOrigin::Cargo))
            }
            FlycheckConfig::CustomCommand { command, args, extra_env, invocation_strategy } => {
                let root = match invocation_strategy {
                    InvocationStrategy::Once => &*self.root,
                    InvocationStrategy::PerWorkspace => {
                        // FIXME: &affected_workspace
                        &*self.root
                    }
                };
                let runnable = project_json::Runnable {
                    program: command.clone(),
                    cwd: Utf8Path::to_owned(root.as_ref()),
                    args: args.clone(),
                    kind: project_json::RunnableKind::Flycheck,
                };

                let label = match scope {
                    FlycheckScope::Workspace => None,
                    // We support substituting both build labels (e.g. buck, bazel) and cargo package ids.
                    // With cargo package ids, you get `cargo check -p path+file:///path/to/rust-analyzer/crates/hir#0.0.0`.
                    // That does work!
                    FlycheckScope::Package { package, .. } => Some(package.as_str()),
                };

                let subs = Substitutions { label, saved_file: saved_file.map(|x| x.as_str()) };
                let cmd = subs.substitute(&runnable, extra_env)?;

                Some((cmd, FlycheckCommandOrigin::CheckOverrideCommand))
            }
        }
    }

    #[track_caller]
    fn send(&self, check_task: FlycheckMessage) {
        self.sender.send(check_task).unwrap();
    }

    fn supports_ownership_wrapper(&self, cargo_options: &CargoOptions) -> bool {
        if let Some(supported) = self.ownership_wrapper_support.get() {
            return supported;
        }
        let rustc = match cargo_options.extra_env.get("RUSTC") {
            Some(Some(rustc)) => Utf8PathBuf::from(rustc),
            Some(None) => Utf8PathBuf::from("rustc"),
            None => Tool::Rustc.path(),
        };
        let mut command = toolchain::command(rustc, &*self.root, &cargo_options.extra_env);
        if let Some(sysroot_root) = &self.sysroot_root
            && !cargo_options.extra_env.contains_key("RUSTUP_TOOLCHAIN")
            && std::env::var_os("RUSTUP_TOOLCHAIN").is_none()
        {
            command.env("RUSTUP_TOOLCHAIN", AsRef::<std::path::Path>::as_ref(sysroot_root));
        }
        let supported = command.arg("-Zhelp").output().is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("borrowck-wrapper-suggestions")
        });
        self.ownership_wrapper_support.set(Some(supported));
        if !supported && !self.warned_unsupported_ownership_wrapper.replace(true) {
            tracing::warn!(
                "the selected rustc does not support -Zborrowck-wrapper-suggestions; using only unvalidated ownership-wrapper assists"
            );
        }
        supported
    }

    fn supports_ownership_events(&self, cargo_options: &CargoOptions) -> bool {
        let rustc = match cargo_options.extra_env.get("RUSTC") {
            Some(Some(rustc)) => Utf8PathBuf::from(rustc),
            Some(None) => Utf8PathBuf::from("rustc"),
            None => Tool::Rustc.path(),
        };
        let mut command = toolchain::command(rustc, &*self.root, &cargo_options.extra_env);
        if let Some(sysroot_root) = &self.sysroot_root
            && !cargo_options.extra_env.contains_key("RUSTUP_TOOLCHAIN")
            && std::env::var_os("RUSTUP_TOOLCHAIN").is_none()
        {
            command.env("RUSTUP_TOOLCHAIN", AsRef::<std::path::Path>::as_ref(sysroot_root));
        }
        let supported = command.arg("-Zhelp").output().is_ok_and(|output| {
            output.status.success() && {
                let output = String::from_utf8_lossy(&output.stdout);
                output.contains("borrowck-ownership-model")
                    || output.contains("borrowck-ownership-events")
            }
        });
        if !supported {
            tracing::warn!(
                "the selected rustc does not support -Zborrowck-ownership-events; using estimated ownership hints"
            );
        }
        supported
    }

    fn configure_ownership_wrapper(
        &self,
        cmd: &mut Command,
        cargo_options: &CargoOptions,
        mode: OwnershipWrapperMode,
    ) {
        let Ok(myself) = std::env::current_exe() else {
            tracing::warn!(
                "failed to locate rust-analyzer executable for ownership-wrapper checks"
            );
            return;
        };
        let original = effective_env(&cargo_options.extra_env, "RUSTC_WORKSPACE_WRAPPER");
        if original.as_deref().is_some_and(|wrapper| wrapper == myself.as_os_str()) {
            tracing::warn!("rust-analyzer is already configured as RUSTC_WORKSPACE_WRAPPER");
            return;
        }
        let ownership_model_directory = ownership_model_directory(&self.root);
        prune_ownership_model_directory(&ownership_model_directory);
        let ownership_events = mode != OwnershipWrapperMode::Suggestions;
        let wrapper = if ownership_events {
            ownership_wrapper_alias(&ownership_model_directory, &myself).unwrap_or_else(|| {
                tracing::warn!(
                    path = %ownership_model_directory.display(),
                    "failed to create stable ownership wrapper alias; cached successful targets may need a source edit before ownership facts appear"
                );
                myself.clone()
            })
        } else {
            myself.clone()
        };
        cmd.env("RUSTC_WORKSPACE_WRAPPER", wrapper);
        cmd.env(
            "RA_RUSTC_WRAPPER",
            match mode {
                OwnershipWrapperMode::Model => "ownership",
                OwnershipWrapperMode::Suggestions => "borrowck-wrapper-suggestions",
                OwnershipWrapperMode::ModelAndSuggestions => "ownership-validation",
            },
        );
        if ownership_events {
            cmd.env("RA_OWNERSHIP_MODEL_DIR", ownership_model_directory);
        } else {
            cmd.env_remove("RA_OWNERSHIP_MODEL_DIR");
        }
        match original {
            Some(original) => cmd.env("RA_ORIGINAL_RUSTC_WORKSPACE_WRAPPER", original),
            None => cmd.env_remove("RA_ORIGINAL_RUSTC_WORKSPACE_WRAPPER"),
        };
    }

    fn report_cached_ownership_artifacts(&self) {
        let FlycheckConfig::Automatic { ownership_visualization: true, .. } = &self.config else {
            return;
        };
        let directory = ownership_model_directory(&self.root);
        let Ok(entries) = std::fs::read_dir(&directory) else {
            return;
        };
        let mut artifacts = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
            .collect::<Vec<_>>();
        artifacts.sort_by_key(|path| {
            (path.metadata().and_then(|metadata| metadata.modified()).ok(), path.clone())
        });
        if !artifacts.is_empty() {
            self.send(FlycheckMessage::OwnershipArtifacts { id: self.id, paths: artifacts });
        }
    }
}

fn ownership_model_directory(root: &AbsPath) -> std::path::PathBuf {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in root.as_str().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    std::env::temp_dir().join(format!("rust-analyzer-ownership-v5-{hash:016x}"))
}

fn ownership_validation_target_directory(
    cargo_options: &CargoOptions,
    ws_target_dir: Option<&Utf8Path>,
) -> Utf8PathBuf {
    cargo_options
        .target_dir_config
        .target_dir(ws_target_dir)
        .map(std::borrow::Cow::into_owned)
        .or_else(|| ws_target_dir.map(Utf8Path::to_owned))
        .unwrap_or_else(|| Utf8PathBuf::from("target"))
        .join("rust-analyzer-ownership-validation-v1")
}

static NEXT_ALIAS_TEMP: AtomicUsize = AtomicUsize::new(0);

fn ownership_wrapper_alias(
    directory: &std::path::Path,
    executable: &std::path::Path,
) -> Option<std::path::PathBuf> {
    std::fs::create_dir_all(directory).ok()?;
    let canonical = std::fs::canonicalize(executable).ok()?;
    let metadata = canonical.metadata().ok()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let identity = stable_cache_hash(
        format!("{}:{}:{modified}", canonical.display(), metadata.len()).as_bytes(),
    );
    let alias = directory.join(format!(
        "rust-analyzer-ownership-wrapper-v5-{identity}{}",
        std::env::consts::EXE_SUFFIX
    ));
    if std::fs::canonicalize(&alias).ok().as_deref() == Some(canonical.as_path()) {
        return Some(alias);
    }
    let temporary = alias.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        NEXT_ALIAS_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&temporary);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&canonical, &temporary).ok()?;
    #[cfg(windows)]
    std::fs::hard_link(&canonical, &temporary).ok()?;
    #[cfg(not(any(unix, windows)))]
    return None;
    if let Err(error) = std::fs::rename(&temporary, &alias) {
        let _ = std::fs::remove_file(&temporary);
        if error.kind() != io::ErrorKind::AlreadyExists
            || std::fs::canonicalize(&alias).ok().as_deref() != Some(canonical.as_path())
        {
            return None;
        }
    }
    Some(alias)
}

fn stable_cache_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn prune_ownership_model_directory(directory: &std::path::Path) {
    const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
    const ABANDONED_TEMP_AGE: Duration = Duration::from_secs(10 * 60);
    let Ok(entries) = std::fs::read_dir(directory) else { return };
    let now = SystemTime::now();
    let mut artifacts = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else { continue };
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if path.extension().is_some_and(|extension| extension == "tmp") {
            if now.duration_since(modified).unwrap_or_default() >= ABANDONED_TEMP_AGE {
                let _ = std::fs::remove_file(path);
            }
        } else if path.extension().is_some_and(|extension| extension == "json") {
            artifacts.push((modified, metadata.len(), path));
        }
    }
    artifacts.sort_by_key(|(modified, _, _)| std::cmp::Reverse(*modified));
    let mut retained = 0_u64;
    for (_, size, path) in artifacts {
        retained = retained.saturating_add(size);
        if retained > MAX_ARTIFACT_BYTES {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn effective_env(extra_env: &FxHashMap<String, Option<String>>, name: &str) -> Option<OsString> {
    match extra_env.get(name) {
        Some(Some(value)) => Some(value.into()),
        Some(None) => None,
        None => std::env::var_os(name),
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum CheckMessage {
    /// A message from `cargo check`, including details like the path
    /// to the relevant `Cargo.toml`.
    CompilerArtifact(cargo_metadata::Artifact),
    /// A diagnostic message from rustc itself.
    Diagnostic { diagnostic: Diagnostic, package_id: Option<PackageSpecifier> },
}

struct CheckParser;

impl CheckParser {
    fn parse_line(&self, line: &str, error: &mut String) -> Option<CheckMessage> {
        let mut deserializer = serde_json::Deserializer::from_str(line);
        deserializer.disable_recursion_limit();
        if let Ok(message) = JsonMessage::deserialize(&mut deserializer) {
            return match message {
                // Skip certain kinds of messages to only spend time on what's useful
                JsonMessage::Cargo(message) => match message {
                    cargo_metadata::Message::CompilerArtifact(artifact) if !artifact.fresh => {
                        Some(CheckMessage::CompilerArtifact(artifact))
                    }
                    cargo_metadata::Message::CompilerMessage(msg) => {
                        Some(CheckMessage::Diagnostic {
                            diagnostic: msg.message,
                            package_id: Some(PackageSpecifier::Cargo {
                                package_id: Arc::new(msg.package_id),
                            }),
                        })
                    }
                    _ => None,
                },
                JsonMessage::Rustc(message) => {
                    Some(CheckMessage::Diagnostic { diagnostic: message, package_id: None })
                }
            };
        }

        error.push_str(line);
        error.push('\n');
        None
    }
}

impl JsonLinesParser<CheckMessage> for CheckParser {
    fn from_line(&self, line: &str, error: &mut String) -> Option<CheckMessage> {
        self.parse_line(line, error)
    }

    fn from_stderr_line(&self, line: &str, error: &mut String) -> Option<CheckMessage> {
        self.parse_line(line, error)
    }

    fn from_eof(&self) -> Option<CheckMessage> {
        None
    }
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum JsonMessage {
    Cargo(cargo_metadata::Message),
    Rustc(Diagnostic),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ide_db::FxHashMap;
    use itertools::Itertools;
    use paths::Utf8Path;
    use project_model::project_json;

    #[test]
    fn test_substitutions() {
        let label = ":label";
        let saved_file = "file.rs";

        // Runnable says it needs both; you need both.
        assert_eq!(test_substitute(None, None, "{label} {saved_file}").as_deref(), None);
        assert_eq!(test_substitute(Some(label), None, "{label} {saved_file}").as_deref(), None);
        assert_eq!(
            test_substitute(None, Some(saved_file), "{label} {saved_file}").as_deref(),
            None
        );
        assert_eq!(
            test_substitute(Some(label), Some(saved_file), "{label} {saved_file}").as_deref(),
            Some("build :label file.rs")
        );

        // Only need label? only need label.
        assert_eq!(test_substitute(None, None, "{label}").as_deref(), None);
        assert_eq!(test_substitute(Some(label), None, "{label}").as_deref(), Some("build :label"),);
        assert_eq!(test_substitute(None, Some(saved_file), "{label}").as_deref(), None,);
        assert_eq!(
            test_substitute(Some(label), Some(saved_file), "{label}").as_deref(),
            Some("build :label"),
        );

        // Only need saved_file
        assert_eq!(test_substitute(None, None, "{saved_file}").as_deref(), None);
        assert_eq!(test_substitute(Some(label), None, "{saved_file}").as_deref(), None);
        assert_eq!(
            test_substitute(None, Some(saved_file), "{saved_file}").as_deref(),
            Some("build file.rs")
        );
        assert_eq!(
            test_substitute(Some(label), Some(saved_file), "{saved_file}").as_deref(),
            Some("build file.rs")
        );

        // Need neither
        assert_eq!(test_substitute(None, None, "xxx").as_deref(), Some("build xxx"));
        assert_eq!(test_substitute(Some(label), None, "xxx").as_deref(), Some("build xxx"));
        assert_eq!(test_substitute(None, Some(saved_file), "xxx").as_deref(), Some("build xxx"));
        assert_eq!(
            test_substitute(Some(label), Some(saved_file), "xxx").as_deref(),
            Some("build xxx")
        );

        // {label} mid-argument substitution
        assert_eq!(
            test_substitute(Some(label), None, "--label={label}").as_deref(),
            Some("build --label=:label")
        );

        // {saved_file} mid-argument substitution
        assert_eq!(
            test_substitute(None, Some(saved_file), "--saved={saved_file}").as_deref(),
            Some("build --saved=file.rs")
        );

        // $saved_file legacy support (no mid-argument substitution, we never supported that)
        assert_eq!(
            test_substitute(None, Some(saved_file), "$saved_file").as_deref(),
            Some("build file.rs")
        );

        fn test_substitute(
            label: Option<&str>,
            saved_file: Option<&str>,
            args: &str,
        ) -> Option<String> {
            Substitutions { label, saved_file }
                .substitute(
                    &project_json::Runnable {
                        program: "build".to_owned(),
                        args: Vec::from_iter(args.split_whitespace().map(ToOwned::to_owned)),
                        cwd: Utf8Path::new("/path").to_owned(),
                        kind: project_json::RunnableKind::Flycheck,
                    },
                    &FxHashMap::default(),
                )
                .map(|command| {
                    command.get_args().map(|x| x.to_string_lossy()).collect_vec().join(" ")
                })
                .map(|args| format!("build {}", args))
        }
    }

    #[test]
    fn test_flycheck_config_display() {
        let clippy = FlycheckConfig::Automatic {
            cargo_options: CargoOptions {
                subcommand: "clippy".to_owned(),
                target_tuples: vec![],
                all_targets: false,
                set_test: false,
                no_default_features: false,
                all_features: false,
                features: vec![],
                extra_args: vec![],
                extra_test_bin_args: vec![],
                extra_env: FxHashMap::default(),
                config_path: None,
                target_dir_config: TargetDirectoryConfig::default(),
            },
            ansi_color_output: true,
            ownership_visualization: false,
            ownership_wrapper_suggestions: false,
        };
        assert_eq!(clippy.to_string(), "cargo clippy");

        let custom_dollar = FlycheckConfig::CustomCommand {
            command: "check".to_owned(),
            args: vec!["--input".to_owned(), "$saved_file".to_owned()],
            extra_env: FxHashMap::default(),
            invocation_strategy: InvocationStrategy::Once,
        };
        assert_eq!(custom_dollar.to_string(), "check --input ...");

        let custom_inline = FlycheckConfig::CustomCommand {
            command: "check".to_owned(),
            args: vec!["--input".to_owned(), "{saved_file}".to_owned()],
            extra_env: FxHashMap::default(),
            invocation_strategy: InvocationStrategy::Once,
        };
        assert_eq!(custom_inline.to_string(), "check --input ...");

        let custom_rs = FlycheckConfig::CustomCommand {
            command: "check".to_owned(),
            args: vec!["--input".to_owned(), "/path/to/file.rs".to_owned()],
            extra_env: FxHashMap::default(),
            invocation_strategy: InvocationStrategy::Once,
        };
        assert_eq!(custom_rs.to_string(), "check --input ...");
    }

    #[test]
    fn ownership_model_cache_is_stable_and_workspace_scoped() {
        let root = AbsPathBuf::assert_utf8(std::env::current_dir().unwrap());
        let first = ownership_model_directory(&root);
        let second = ownership_model_directory(&root);
        let other = ownership_model_directory(&root.join("another-workspace"));

        assert_eq!(first, second);
        assert_ne!(first, other);
        assert!(
            first.file_name().unwrap().to_string_lossy().starts_with("rust-analyzer-ownership-")
        );
    }

    #[test]
    fn ownership_wrapper_alias_is_safe_for_concurrent_flychecks() {
        let directory = std::env::temp_dir().join(format!(
            "rust-analyzer-wrapper-alias-test-{}-{}",
            std::process::id(),
            NEXT_ALIAS_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let executable = std::env::current_exe().unwrap();
        let handles = (0..8)
            .map(|_| {
                let directory = directory.clone();
                let executable = executable.clone();
                std::thread::spawn(move || ownership_wrapper_alias(&directory, &executable))
            })
            .collect::<Vec<_>>();
        let aliases = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().expect("alias creation failed"))
            .collect::<Vec<_>>();
        assert!(aliases.windows(2).all(|aliases| aliases[0] == aliases[1]));
        assert_eq!(
            std::fs::canonicalize(&aliases[0]).unwrap(),
            std::fs::canonicalize(executable).unwrap()
        );
        let _ = std::fs::remove_dir_all(directory);
    }
}
