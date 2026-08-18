//! The main loop of `rust-analyzer` responsible for dispatching LSP
//! requests/replies and notifications back to the client.

use std::{
    fmt,
    ops::Div as _,
    panic::AssertUnwindSafe,
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, never, select};
use ide::TextSize;
use ide_db::{
    FxHashMap,
    base_db::{SourceDatabase, VfsPath},
};
use lsp_server::{Connection, Notification, Request};
use lsp_types::{Notification as _, TextDocumentIdentifier};
use stdx::thread::ThreadIntent;
use tracing::{Level, error, span};
use vfs::{AbsPathBuf, FileId, loader::LoadingProgress};

use crate::{
    config::Config,
    diagnostics::{
        DiagnosticsGeneration, NativeDiagnosticsFetchKind, OwnershipDestination,
        OwnershipDiagnostic, OwnershipEvent, OwnershipEventPayload, OwnershipModelArtifact,
        OwnershipModelLoanPoint, OwnershipModelPointer, OwnershipModelSource, OwnershipModelSpan,
        OwnershipState, OwnershipTutorialBinding, OwnershipTutorialBlock, OwnershipTutorialBody,
        OwnershipTutorialLoan, OwnershipTutorialLoanPoint, OwnershipTutorialMemoryGraph,
        OwnershipTutorialMemoryNode, OwnershipTutorialModel, OwnershipTutorialSnapshot,
        fetch_native_diagnostics, stable_source_hash,
    },
    discover::{DiscoverArgument, DiscoverCommand, DiscoverProjectMessage},
    flycheck::{self, ClearDiagnosticsKind, ClearScope, FlycheckMessage},
    global_state::{
        FetchBuildDataResponse, FetchWorkspaceRequest, FetchWorkspaceResponse, GlobalState,
        file_id_to_url, url_to_file_id,
    },
    handlers::{
        dispatch::{NotificationDispatcher, RequestDispatcher},
        request::empty_diagnostic_report,
    },
    lsp::{
        from_proto, to_proto,
        utils::{Progress, notification_is},
    },
    lsp_ext,
    reload::{BuildDataProgress, ProcMacroProgress, ProjectWorkspaceProgress},
    test_runner::{CargoTestMessage, CargoTestOutput, TestState},
};

pub fn main_loop(config: Config, connection: Connection) -> anyhow::Result<()> {
    tracing::info!("initial config: {:#?}", config);

    // Windows scheduler implements priority boosts: if thread waits for an
    // event (like a condvar), and event fires, priority of the thread is
    // temporary bumped. This optimization backfires in our case: each time the
    // `main_loop` schedules a task to run on a threadpool, the worker threads
    // gets a higher priority, and (on a machine with fewer cores) displaces the
    // main loop! We work around this by marking the main loop as a
    // higher-priority thread.
    //
    // https://docs.microsoft.com/en-us/windows/win32/procthread/scheduling-priorities
    // https://docs.microsoft.com/en-us/windows/win32/procthread/priority-boosts
    // https://github.com/rust-lang/rust-analyzer/issues/2835
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::*;
        let thread = GetCurrentThread();
        let thread_priority_above_normal = 1;
        SetThreadPriority(thread, thread_priority_above_normal);
    }

    #[cfg(feature = "dhat")]
    {
        if let Some(dhat_output_file) = config.dhat_output_file() {
            *crate::DHAT_PROFILER.lock().unwrap() =
                Some(dhat::Profiler::builder().file_name(&dhat_output_file).build());
        }
    }

    GlobalState::new(connection.sender, config).run(connection.receiver)
}

fn read_ownership_model_artifact(path: &std::path::Path) -> anyhow::Result<OwnershipModelArtifact> {
    let file = std::fs::File::open(path)?;
    Ok(serde_json::from_reader(file)?)
}

enum Event {
    Lsp(lsp_server::Message),
    Task(Task),
    DeferredTask(DeferredTask),
    Vfs(vfs::loader::Message),
    Flycheck(FlycheckMessage),
    TestResult(CargoTestMessage),
    DiscoverProject(DiscoverProjectMessage),
    FetchWorkspaces(FetchWorkspaceRequest),
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::Lsp(_) => write!(f, "Event::Lsp"),
            Event::Task(_) => write!(f, "Event::Task"),
            Event::Vfs(_) => write!(f, "Event::Vfs"),
            Event::Flycheck(_) => write!(f, "Event::Flycheck"),
            Event::DeferredTask(_) => write!(f, "Event::DeferredTask"),
            Event::TestResult(_) => write!(f, "Event::TestResult"),
            Event::DiscoverProject(_) => write!(f, "Event::DiscoverProject"),
            Event::FetchWorkspaces(_) => write!(f, "Event::FetchWorkspaces"),
        }
    }
}

#[derive(Debug)]
pub(crate) enum DeferredTask {
    CheckIfIndexed(lsp_types::Uri),
    CheckProcMacroSources(Vec<FileId>),
}

#[derive(Debug)]
pub(crate) enum DiagnosticsTaskKind {
    Syntax(DiagnosticsGeneration, Vec<(FileId, Vec<lsp_types::Diagnostic>)>),
    Semantic(DiagnosticsGeneration, Vec<(FileId, Vec<lsp_types::Diagnostic>)>),
}

#[derive(Debug)]
pub(crate) enum Task {
    Response(lsp_server::Response),
    DiscoverLinkedProjects(DiscoverProjectParam),
    Retry(lsp_server::Request),
    Diagnostics(DiagnosticsTaskKind),
    DiscoverTest(lsp_ext::DiscoverTestResults),
    PrimeCaches(PrimeCachesProgress),
    FetchWorkspace(ProjectWorkspaceProgress),
    FetchBuildData(BuildDataProgress),
    LoadProcMacros(ProcMacroProgress),
    OwnershipArtifacts(PreparedOwnershipBatch),
    // FIXME: Remove this in favor of a more general QueuedTask, see `handle_did_save_text_document`
    BuildDepsHaveChanged,
}

pub(crate) struct PreparedOwnershipBatch {
    id: usize,
    package_id: Option<flycheck::PackageSpecifier>,
    sequence: u64,
    files: Vec<PreparedOwnershipFile>,
    preparation_time: Duration,
}

impl fmt::Debug for PreparedOwnershipBatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedOwnershipBatch")
            .field("id", &self.id)
            .field("sequence", &self.sequence)
            .field("files", &self.files.len())
            .field("preparation_time", &self.preparation_time)
            .finish()
    }
}

struct PreparedOwnershipFile {
    uri: lsp_types::Uri,
    file_id: FileId,
    vfs_hash: u64,
    source_hash: String,
    schema_version: u32,
    events: Vec<OwnershipEvent>,
    model: Option<OwnershipTutorialModel>,
}

#[derive(Debug)]
pub(crate) enum DiscoverProjectParam {
    Buildfile(AbsPathBuf),
    Path(AbsPathBuf),
}

#[derive(Debug)]
pub(crate) enum PrimeCachesProgress {
    Begin,
    Report(ide::ParallelPrimeCachesProgress),
    End { cancelled: bool },
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let debug_non_verbose = |not: &Notification, f: &mut fmt::Formatter<'_>| {
            f.debug_struct("Notification").field("method", &not.method).finish()
        };

        match self {
            Event::Lsp(lsp_server::Message::Notification(not))
                if (notification_is::<lsp_types::DidOpenTextDocumentNotification>(not)
                    || notification_is::<lsp_types::DidChangeTextDocumentNotification>(not)) =>
            {
                return debug_non_verbose(not, f);
            }
            Event::Task(Task::Response(resp)) => {
                return f
                    .debug_struct("Response")
                    .field("id", &resp.id)
                    .field("error", &resp.error)
                    .finish();
            }
            _ => (),
        }

        match self {
            Event::Lsp(it) => fmt::Debug::fmt(it, f),
            Event::Task(it) => fmt::Debug::fmt(it, f),
            Event::DeferredTask(it) => fmt::Debug::fmt(it, f),
            Event::Vfs(it) => fmt::Debug::fmt(it, f),
            Event::Flycheck(it) => fmt::Debug::fmt(it, f),
            Event::TestResult(it) => fmt::Debug::fmt(it, f),
            Event::DiscoverProject(it) => fmt::Debug::fmt(it, f),
            Event::FetchWorkspaces(it) => fmt::Debug::fmt(it, f),
        }
    }
}

impl GlobalState {
    fn run(mut self, inbox: Receiver<lsp_server::Message>) -> anyhow::Result<()> {
        self.update_status_or_notify();

        if self.config.did_save_text_document_dynamic_registration() {
            let additional_patterns = self
                .config
                .discover_workspace_config()
                .map(|cfg| cfg.files_to_watch.clone().into_iter())
                .into_iter()
                .flatten()
                .map(|f| format!("**/{f}"));
            self.register_did_save_capability(additional_patterns);
        }

        if self.config.discover_workspace_config().is_none() {
            self.fetch_workspaces_queue.request_op(
                "startup".to_owned(),
                FetchWorkspaceRequest { path: None, force_crate_graph_reload: false },
            );
            if let Some((cause, FetchWorkspaceRequest { path, force_crate_graph_reload })) =
                self.fetch_workspaces_queue.should_start_op()
            {
                self.fetch_workspaces(cause, path, force_crate_graph_reload);
            }
        }

        while let Ok(event) = self.next_event(&inbox) {
            let Some(event) = event else {
                anyhow::bail!("client exited without proper shutdown sequence");
            };
            if matches!(
                &event,
                Event::Lsp(lsp_server::Message::Notification(Notification { method, .. }))
                if method == lsp_types::ExitNotification::METHOD.as_str()
            ) {
                return Ok(());
            }
            self.handle_event(event);
        }

        Err(anyhow::anyhow!("A receiver has been dropped, something panicked!"))
    }

    fn register_did_save_capability(&mut self, additional_patterns: impl Iterator<Item = String>) {
        let additional_filters = additional_patterns.map(|pattern| {
            lsp_types::DocumentFilter::TextDocumentFilter(lsp_types::TextDocumentFilter::Pattern(
                lsp_types::TextDocumentFilterPattern {
                    language: None,
                    scheme: None,
                    pattern: pattern.into(),
                },
            ))
        });

        let mut selectors = vec![
            lsp_types::DocumentFilter::TextDocumentFilter(lsp_types::TextDocumentFilter::Pattern(
                lsp_types::TextDocumentFilterPattern {
                    language: None,
                    scheme: None,
                    pattern: "**/*.rs".to_owned().into(),
                },
            )),
            lsp_types::DocumentFilter::TextDocumentFilter(lsp_types::TextDocumentFilter::Pattern(
                lsp_types::TextDocumentFilterPattern {
                    language: None,
                    scheme: None,
                    pattern: "**/Cargo.toml".to_owned().into(),
                },
            )),
            lsp_types::DocumentFilter::TextDocumentFilter(lsp_types::TextDocumentFilter::Pattern(
                lsp_types::TextDocumentFilterPattern {
                    language: None,
                    scheme: None,
                    pattern: "**/Cargo.lock".to_owned().into(),
                },
            )),
        ];
        selectors.extend(additional_filters);

        let save_registration_options = lsp_types::TextDocumentSaveRegistrationOptions {
            save_options: lsp_types::SaveOptions { include_text: Some(false) },
            text_document_registration_options: lsp_types::TextDocumentRegistrationOptions {
                document_selector: Some(selectors),
            },
        };

        let registration = lsp_types::Registration {
            id: "textDocument/didSave".to_owned(),
            method: "textDocument/didSave".to_owned(),
            register_options: Some(serde_json::to_value(save_registration_options).unwrap()),
        };
        self.send_request::<lsp_types::RegistrationRequest>(
            lsp_types::RegistrationParams { registrations: vec![registration] },
            |_, _| (),
        );
    }

    fn next_event(
        &mut self,
        inbox: &Receiver<lsp_server::Message>,
    ) -> Result<Option<Event>, crossbeam_channel::RecvError> {
        // Make sure we reply to formatting requests ASAP so the editor doesn't block
        if let Ok(task) = self.fmt_pool.receiver.try_recv() {
            return Ok(Some(Event::Task(task)));
        }

        select! {
            recv(inbox) -> msg =>
                return Ok(msg.ok().map(Event::Lsp)),

            recv(self.task_pool.receiver) -> task =>
                task.map(Event::Task),

            recv(self.deferred_task_queue.receiver) -> task =>
                task.map(Event::DeferredTask),

            recv(self.fmt_pool.receiver) -> task =>
                task.map(Event::Task),

            recv(self.loader.receiver) -> task =>
                task.map(Event::Vfs),

            recv(self.flycheck_receiver) -> task =>
                task.map(Event::Flycheck),

            recv(self.test_run_receiver) -> task =>
                task.map(Event::TestResult),

            recv(self.discover_receiver) -> task =>
                task.map(Event::DiscoverProject),

            recv(self.fetch_ws_receiver.as_ref().map_or(&never(), |(chan, _)| chan)) -> _instant => {
                Ok(Event::FetchWorkspaces(self.fetch_ws_receiver.take().unwrap().1))
            },
        }
        .map(Some)
    }

    fn handle_event(&mut self, event: Event) {
        let loop_start = Instant::now();
        let _p = tracing::info_span!("GlobalState::handle_event", event = %event).entered();

        let event_dbg_msg = format!("{event:?}");
        tracing::debug!(?event, "handle_event");

        let was_quiescent = self.is_quiescent();

        let mut cancellation_time = None;
        match event {
            Event::Lsp(msg) => match msg {
                lsp_server::Message::Request(req) => self.on_new_request(loop_start, req),
                lsp_server::Message::Notification(not) => self.on_notification(not),
                lsp_server::Message::Response(resp) => self.complete_request(resp),
            },
            Event::DeferredTask(task) => {
                let _p = tracing::info_span!("GlobalState::handle_event/queued_task").entered();
                self.handle_deferred_task(task);
                // Coalesce multiple deferred task events into one loop turn
                while loop_start.elapsed() < Duration::from_millis(50)
                    && let Ok(task) = self.deferred_task_queue.receiver.try_recv()
                {
                    self.handle_deferred_task(task);
                }
            }
            Event::Task(task) => {
                let _p = tracing::info_span!("GlobalState::handle_event/task").entered();
                let mut prime_caches_progress = Vec::new();

                // Ownership artifacts are intentionally emitted one source file at a time. Do
                // not undo that back-pressure by coalescing all of those files into this same
                // main-loop turn: a large crate can otherwise monopolize the event loop even
                // though every individual cache commit is small.
                let mut should_yield = matches!(task, Task::OwnershipArtifacts(_));
                cancellation_time = self.handle_task(&mut prime_caches_progress, task);
                // Coalesce multiple task events into one loop turn
                while !should_yield
                    && loop_start.elapsed() < Duration::from_millis(50)
                    && let Ok(task) = self.task_pool.receiver.try_recv()
                {
                    should_yield = matches!(task, Task::OwnershipArtifacts(_));
                    self.handle_task(&mut prime_caches_progress, task);
                }

                let title = "Indexing";
                let cancel_token = || Some("rustAnalyzer/cachePriming".to_owned());

                let mut last_report = None;
                for progress in prime_caches_progress {
                    match progress {
                        PrimeCachesProgress::Begin => {
                            self.report_progress(
                                title,
                                Progress::Begin,
                                None,
                                Some(0.0),
                                cancel_token(),
                            );
                        }
                        PrimeCachesProgress::Report(report) => {
                            let message = match &*report.crates_currently_indexing {
                                [crate_name] => Some(format!(
                                    "{}/{} ({})",
                                    report.crates_done,
                                    report.crates_total,
                                    crate_name.as_str(),
                                )),
                                [crate_name, rest @ ..] => Some(format!(
                                    "{}/{} ({} + {} more)",
                                    report.crates_done,
                                    report.crates_total,
                                    crate_name.as_str(),
                                    rest.len()
                                )),
                                _ => None,
                            };

                            // Don't send too many notifications while batching, sending progress reports
                            // serializes notifications on the mainthread at the moment which slows us down
                            last_report = Some((
                                message,
                                Progress::fraction(report.crates_done, report.crates_total),
                                report.work_type,
                            ));
                        }
                        PrimeCachesProgress::End { cancelled } => {
                            self.analysis_host.trigger_garbage_collection();
                            // The explicit post-prime collection already covered this revision.
                            // Without updating the marker the quiescent tail of this same loop
                            // turn immediately performs the same expensive collection again.
                            self.last_gc_revision =
                                self.analysis_host.raw_database().nonce_and_revision().1;
                            self.prime_caches_queue.op_completed(());
                            if cancelled {
                                self.prime_caches_queue
                                    .request_op("restart after cancellation".to_owned(), ());
                            } else {
                                if self.config.check_on_save(None)
                                    && self.config.flycheck_workspace(None)
                                    && !self.fetch_build_data_queue.op_requested()
                                {
                                    // Priming finished; now run the deferred initial workspace flycheck
                                    // (kept off the critical path so `cargo check` doesn't contend with
                                    // cache priming for CPU).
                                    self.flycheck
                                        .iter()
                                        .for_each(|flycheck| flycheck.restart_workspace(None));
                                }
                                tracing::info!("cache priming completed successfully");
                            }
                            if let Some((message, fraction, title)) = last_report.take() {
                                self.report_progress(
                                    title,
                                    Progress::Report,
                                    message,
                                    Some(fraction),
                                    cancel_token(),
                                );
                            }
                            self.report_progress(
                                title,
                                Progress::End,
                                None,
                                Some(1.0),
                                cancel_token(),
                            );
                        }
                    };
                }
                if let Some((message, fraction, title)) = last_report.take() {
                    self.report_progress(
                        title,
                        Progress::Report,
                        message,
                        Some(fraction),
                        cancel_token(),
                    );
                }
            }
            Event::Vfs(message) => {
                let _p = tracing::info_span!("GlobalState::handle_event/vfs").entered();
                let mut last_progress_report = None;
                self.handle_vfs_msg(message, &mut last_progress_report);
                // Coalesce many VFS event into a single loop turn
                while loop_start.elapsed() < Duration::from_millis(50)
                    && let Ok(message) = self.loader.receiver.try_recv()
                {
                    self.handle_vfs_msg(message, &mut last_progress_report);
                }
                if let Some((message, fraction)) = last_progress_report {
                    self.report_progress(
                        "Roots Scanned",
                        Progress::Report,
                        Some(message),
                        Some(fraction),
                        None,
                    );
                }
            }
            Event::Flycheck(message) => {
                let mut cargo_finished = false;
                self.handle_flycheck_msg(message, &mut cargo_finished);
                // Coalesce many flycheck updates into a single loop turn
                while loop_start.elapsed() < Duration::from_millis(50)
                    && let Ok(message) = self.flycheck_receiver.try_recv()
                {
                    self.handle_flycheck_msg(message, &mut cargo_finished);
                }
                if cargo_finished {
                    self.send_request::<lsp_types::DiagnosticRefreshRequest>((), |_, _| ());
                }
            }
            Event::TestResult(message) => {
                let _p = tracing::info_span!("GlobalState::handle_event/test_result").entered();
                self.handle_cargo_test_msg(message);
                // Coalesce many test result event into a single loop turn
                while loop_start.elapsed() < Duration::from_millis(50)
                    && let Ok(message) = self.test_run_receiver.try_recv()
                {
                    self.handle_cargo_test_msg(message);
                }
            }
            Event::DiscoverProject(message) => {
                self.handle_discover_msg(message);
                // Coalesce many project discovery events into a single loop turn.
                while loop_start.elapsed() < Duration::from_millis(50)
                    && let Ok(message) = self.discover_receiver.try_recv()
                {
                    self.handle_discover_msg(message);
                }
            }
            Event::FetchWorkspaces(req) => {
                self.fetch_workspaces_queue.request_op("project structure change".to_owned(), req)
            }
        }
        let event_handling_duration = loop_start.elapsed();
        let ((state_changed, changes_cancellation_time), memdocs_added_or_removed) =
            if self.vfs_done {
                if let Some(cause) = self.wants_to_switch.take() {
                    cancellation_time = match (cancellation_time, self.switch_workspaces(cause)) {
                        (Some(a), Some(b)) => Some(a + b),
                        (Some(d), None) | (None, Some(d)) => Some(d),
                        (None, None) => None,
                    };
                }
                (self.process_changes(), self.mem_docs.take_changes())
            } else {
                ((false, None), false)
            };
        cancellation_time = match (cancellation_time, changes_cancellation_time) {
            (Some(a), Some(b)) => Some(a + b),
            (Some(d), None) | (None, Some(d)) => Some(d),
            (None, None) => None,
        };

        let mut gc_elapsed = None;
        if self.is_quiescent() {
            let became_quiescent = !was_quiescent;
            if became_quiescent {
                // delay initial cache priming until proc macros are loaded, or we will load up a bunch of garbage into salsa
                let proc_macros_loaded = self.config.prefill_caches()
                    && (!self.config.expand_proc_macros()
                        || self.fetch_proc_macros_queue.last_op_result().copied().unwrap_or(false));
                if proc_macros_loaded {
                    self.prime_caches_queue.request_op("became quiescent".to_owned(), ());
                }
                if self.config.check_on_save(None)
                    && self.config.flycheck_workspace(None)
                    && !self.fetch_build_data_queue.op_requested()
                {
                    if !self.config.prefill_caches() {
                        self.flycheck.iter().for_each(|flycheck| flycheck.restart_workspace(None));
                    } else if proc_macros_loaded
                        && !self.prime_caches_queue.op_in_progress()
                        && !self.prime_caches_queue.op_requested()
                    {
                        self.flycheck.iter().for_each(|flycheck| flycheck.restart_workspace(None));
                    }
                }
            }

            let client_refresh = became_quiescent || state_changed;
            if client_refresh {
                // Refresh semantic tokens if the client supports it.
                if self.config.semantic_tokens_refresh() {
                    self.semantic_tokens_cache.lock().clear();
                    self.send_request::<lsp_types::SemanticTokensRefreshRequest>((), |_, _| ());
                }

                // Refresh code lens if the client supports it.
                if self.config.code_lens_refresh() {
                    self.send_request::<lsp_types::CodeLensRefreshRequest>((), |_, _| ());
                }

                // Refresh inlay hints if the client supports it.
                if self.config.inlay_hints_refresh() {
                    self.send_request::<lsp_types::InlayHintRefreshRequest>((), |_, _| ());
                }

                if self.config.diagnostics_refresh() {
                    self.send_request::<lsp_types::DiagnosticRefreshRequest>((), |_, _| ());
                }
            }

            let project_or_mem_docs_changed =
                became_quiescent || state_changed || memdocs_added_or_removed;
            if project_or_mem_docs_changed
                && !self.config.text_document_diagnostic()
                && self.config.publish_diagnostics(None)
            {
                self.update_diagnostics();
            }
            if project_or_mem_docs_changed && self.config.test_explorer() {
                self.update_tests();
            }

            let current_revision = self.analysis_host.raw_database().nonce_and_revision().1;
            // no work is currently being done, now we can block a bit and clean up our garbage
            if self.task_pool.handle.is_empty()
                && self.fmt_pool.handle.is_empty()
                && current_revision != self.last_gc_revision
            {
                let gc_start = Instant::now();
                self.analysis_host.trigger_garbage_collection();
                self.last_gc_revision = self.analysis_host.raw_database().nonce_and_revision().1;
                gc_elapsed = Some(gc_start.elapsed());
            }
        }

        self.cleanup_discover_handles();

        if let Some(diagnostic_changes) = self.diagnostics.take_changes() {
            for file_id in diagnostic_changes {
                let uri = file_id_to_url(&self.vfs.read().0, file_id);
                let version = from_proto::vfs_path(&uri)
                    .ok()
                    .and_then(|path| self.mem_docs.get(&path).map(|it| it.version));

                let diagnostics =
                    self.diagnostics.diagnostics_for(file_id).cloned().collect::<Vec<_>>();
                self.publish_diagnostics(uri, version, diagnostics);
            }
        }

        if (self.config.cargo_autoreload_config(None)
            || self.config.discover_workspace_config().is_some())
            && let Some((cause, FetchWorkspaceRequest { path, force_crate_graph_reload })) =
                self.fetch_workspaces_queue.should_start_op()
        {
            self.fetch_workspaces(cause, path, force_crate_graph_reload);
        }

        if !self.fetch_workspaces_queue.op_in_progress() {
            if let Some((cause, ())) = self.fetch_build_data_queue.should_start_op() {
                self.fetch_build_data(cause);
            } else if let Some((cause, (change, paths))) =
                self.fetch_proc_macros_queue.should_start_op()
            {
                self.fetch_proc_macros(cause, change, paths);
            }
        }

        if let Some((cause, ())) = self.prime_caches_queue.should_start_op() {
            self.prime_caches(cause);
        }

        self.update_status_or_notify();

        let loop_duration = loop_start.elapsed();
        if loop_duration > Duration::from_millis(100) && was_quiescent {
            tracing::warn!(
                "overly long loop turn took {loop_duration:?}:\n\
                (event handling took {event_handling_duration:?}): {event_dbg_msg}\n\
                (cancellation took {cancellation_time:?})
                (garbage collection took {gc_elapsed:?})"
            );
            self.poke_rust_analyzer_developer(format!(
                "overly long loop turn took {loop_duration:?}:\n\
                (event handling took {event_handling_duration:?}): {event_dbg_msg}\n\
                (cancellation took {cancellation_time:?})
                (garbage collection took {gc_elapsed:?})"
            ));
        }
    }

    fn prime_caches(&mut self, cause: String) {
        let scope = self.compute_priming_scope();
        tracing::debug!(%cause, scope_size = scope.len(), "will prime caches");
        let num_worker_threads = self.config.prime_caches_num_threads();

        self.task_pool.handle.spawn_with_sender(ThreadIntent::Worker, {
            let analysis = AssertUnwindSafe(self.snapshot().analysis);
            move |sender| {
                sender.send(Task::PrimeCaches(PrimeCachesProgress::Begin)).unwrap();
                let res = analysis.parallel_prime_caches(&scope, num_worker_threads, |progress| {
                    let report = PrimeCachesProgress::Report(progress);
                    sender.send(Task::PrimeCaches(report)).unwrap();
                });
                sender
                    .send(Task::PrimeCaches(PrimeCachesProgress::End { cancelled: res.is_err() }))
                    .unwrap();
            }
        });
    }

    fn update_diagnostics(&mut self) {
        let db = self.analysis_host.raw_database();
        let generation = self.diagnostics.next_generation();
        let subscriptions = {
            let vfs = &self.vfs.read().0;
            self.mem_docs
                .iter()
                .map(|path| vfs.file_id(path).unwrap())
                .filter_map(|(file_id, excluded)| {
                    (excluded == vfs::FileExcluded::No).then_some(file_id)
                })
                .filter(|&file_id| {
                    let source_root_id = db.file_source_root(file_id).source_root_id(db);
                    let source_root = db.source_root(source_root_id).source_root(db);
                    // Only publish diagnostics for files in the workspace, not from crates.io deps
                    // or the sysroot.
                    // While theoretically these should never have errors, we have quite a few false
                    // positives particularly in the stdlib, and those diagnostics would stay around
                    // forever if we emitted them here.
                    !source_root.is_library
                })
                .collect::<std::sync::Arc<_>>()
        };
        tracing::trace!("updating notifications for {:?}", subscriptions);
        // Split up the work on multiple threads, but we don't wanna fill the entire task pool with
        // diagnostic tasks, so we limit the number of tasks to a quarter of the total thread pool.
        let max_tasks = self.config.main_loop_num_threads().div(4).max(1);
        let chunk_length = subscriptions.len() / max_tasks;
        let remainder = subscriptions.len() % max_tasks;

        let mut start = 0;
        for task_idx in 0..max_tasks {
            let extra = if task_idx < remainder { 1 } else { 0 };
            let end = start + chunk_length + extra;
            let slice = start..end;
            if slice.is_empty() {
                break;
            }
            // Diagnostics are triggered by the user typing
            // so we run them on a latency sensitive thread.
            let snapshot = self.snapshot();
            self.task_pool.handle.spawn_with_sender(ThreadIntent::LatencySensitive, {
                let subscriptions = subscriptions.clone();
                // Do not fetch semantic diagnostics (and populate query results) if we haven't even
                // loaded the initial workspace yet.
                //
                // Only fetch semantic diagnostics when
                // - we have fully populated the VFS
                // - have a workspace
                // - have finished fetching the build data once
                // - and have finished loading the proc-macros once
                let fetch_semantic = self.vfs_done
                    && self.fetch_workspaces_queue.last_op_result().is_some()
                    && (!self.config.run_build_scripts(None)
                        || (self.fetch_build_data_queue.last_op_result().is_none()
                            && !self.fetch_build_data_queue.op_in_progress()))
                    && (!self.config.expand_proc_macros()
                        || (self.fetch_proc_macros_queue.last_op_result().is_none()
                            && !self.fetch_proc_macros_queue.op_in_progress()));
                move |sender| {
                    // We aren't observing the semantics token cache here
                    let snapshot = AssertUnwindSafe(&snapshot);
                    let diags = std::panic::catch_unwind(|| {
                        fetch_native_diagnostics(
                            &snapshot,
                            subscriptions.clone(),
                            slice.clone(),
                            NativeDiagnosticsFetchKind::Syntax,
                        )
                    })
                    .unwrap_or_else(|_| {
                        subscriptions.iter().map(|&id| (id, Vec::new())).collect::<Vec<_>>()
                    });
                    sender
                        .send(Task::Diagnostics(DiagnosticsTaskKind::Syntax(generation, diags)))
                        .unwrap();

                    if fetch_semantic {
                        let diags = std::panic::catch_unwind(|| {
                            fetch_native_diagnostics(
                                &snapshot,
                                subscriptions.clone(),
                                slice.clone(),
                                NativeDiagnosticsFetchKind::Semantic,
                            )
                        })
                        .unwrap_or_else(|_| {
                            subscriptions.iter().map(|&id| (id, Vec::new())).collect::<Vec<_>>()
                        });
                        sender
                            .send(Task::Diagnostics(DiagnosticsTaskKind::Semantic(
                                generation, diags,
                            )))
                            .unwrap();
                    }
                }
            });
            start = end;
        }
    }

    fn update_tests(&mut self) {
        if !self.vfs_done {
            return;
        }
        let db = self.analysis_host.raw_database();
        let subscriptions = self
            .mem_docs
            .iter()
            .map(|path| self.vfs.read().0.file_id(path).unwrap())
            .filter_map(|(file_id, excluded)| {
                (excluded == vfs::FileExcluded::No).then_some(file_id)
            })
            .filter(|&file_id| {
                let source_root_id = db.file_source_root(file_id).source_root_id(db);
                let source_root = db.source_root(source_root_id).source_root(db);
                !source_root.is_library
            })
            .collect::<Vec<_>>();
        tracing::trace!("updating tests for {:?}", subscriptions);

        // Updating tests are triggered by the user typing
        // so we run them on a latency sensitive thread.
        self.task_pool.handle.spawn(ThreadIntent::LatencySensitive, {
            let snapshot = self.snapshot();
            move || {
                let tests = subscriptions
                    .iter()
                    .copied()
                    .filter_map(|f| snapshot.analysis.discover_tests_in_file(f).ok())
                    .flatten()
                    .collect::<Vec<_>>();

                Task::DiscoverTest(lsp_ext::DiscoverTestResults {
                    tests: tests
                        .into_iter()
                        .filter_map(|t| {
                            let line_index = t.file.and_then(|f| snapshot.file_line_index(f).ok());
                            to_proto::test_item(&snapshot, t, line_index.as_ref())
                        })
                        .collect(),
                    scope: None,
                    scope_file: Some(
                        subscriptions
                            .into_iter()
                            .map(|f| TextDocumentIdentifier { uri: to_proto::url(&snapshot, f) })
                            .collect(),
                    ),
                })
            }
        });
    }

    fn update_status_or_notify(&mut self) {
        let status = self.current_status();
        if self.last_reported_status != status {
            self.last_reported_status = status.clone();

            if self.config.server_status_notification() {
                self.send_notification::<lsp_ext::ServerStatusNotification>(status);
            } else if let (
                health @ (lsp_ext::Health::Warning | lsp_ext::Health::Error),
                Some(message),
            ) = (status.health, &status.message)
                && self.last_reported_status.message != status.message
            {
                let open_log_button = tracing::enabled!(tracing::Level::ERROR)
                    && (self.fetch_build_data_error().is_err()
                        || self.fetch_workspace_error().is_err());
                self.show_message(
                    match health {
                        lsp_ext::Health::Ok => lsp_types::MessageType::Info,
                        lsp_ext::Health::Warning => lsp_types::MessageType::Warning,
                        lsp_ext::Health::Error => lsp_types::MessageType::Error,
                    },
                    message.clone(),
                    open_log_button,
                );
            }
        }
    }

    fn handle_task(
        &mut self,
        prime_caches_progress: &mut Vec<PrimeCachesProgress>,
        task: Task,
    ) -> Option<Duration> {
        let mut cancellation_time = None;
        match task {
            Task::Response(response) => self.respond(response),
            // Only retry requests that haven't been cancelled. Otherwise we do unnecessary work.
            Task::Retry(req) if !self.is_completed(&req) => self.on_request(req),
            Task::Retry(_) => (),
            Task::Diagnostics(kind) => {
                self.diagnostics.set_native_diagnostics(kind);
            }
            Task::OwnershipArtifacts(batch) => {
                self.commit_ownership_model_artifacts(batch);
            }
            Task::PrimeCaches(progress) => match progress {
                PrimeCachesProgress::Begin => prime_caches_progress.push(progress),
                PrimeCachesProgress::Report(_) => {
                    match prime_caches_progress.last_mut() {
                        Some(last @ PrimeCachesProgress::Report(_)) => {
                            // Coalesce subsequent update events.
                            *last = progress;
                        }
                        _ => prime_caches_progress.push(progress),
                    }
                }
                PrimeCachesProgress::End { .. } => prime_caches_progress.push(progress),
            },
            Task::FetchWorkspace(progress) => {
                let (state, msg) = match progress {
                    ProjectWorkspaceProgress::Begin => (Progress::Begin, None),
                    ProjectWorkspaceProgress::Report(msg) => (Progress::Report, Some(msg)),
                    ProjectWorkspaceProgress::End(workspaces, force_crate_graph_reload) => {
                        let resp = FetchWorkspaceResponse { workspaces, force_crate_graph_reload };
                        self.fetch_workspaces_queue.op_completed(resp);
                        if let Err(e) = self.fetch_workspace_error() {
                            error!("FetchWorkspaceError: {e}");
                        }
                        self.wants_to_switch = Some("fetched workspace".to_owned());
                        self.diagnostics.clear_check_all();
                        (Progress::End, None)
                    }
                };

                self.report_progress("Fetching", state, msg, None, None);
            }
            Task::DiscoverLinkedProjects(arg) => {
                if let Some(cfg) = self.config.discover_workspace_config() {
                    let command = cfg.command.clone();
                    let discover = DiscoverCommand::new(self.discover_sender.clone(), command);

                    let discover_path = match &arg {
                        DiscoverProjectParam::Buildfile(it) => it,
                        DiscoverProjectParam::Path(it) => it,
                    };
                    let current_dir =
                        self.config.workspace_root_for(discover_path.as_path()).clone();

                    let arg = match arg {
                        DiscoverProjectParam::Buildfile(it) => DiscoverArgument::Buildfile(it),
                        DiscoverProjectParam::Path(it) => DiscoverArgument::Path(it),
                    };

                    match discover.spawn(arg, current_dir.as_ref()) {
                        Ok(handle) => {
                            if self.discover_jobs_active == 0 {
                                let title = &cfg.progress_label.clone();
                                self.report_progress(title, Progress::Begin, None, None, None);
                            }
                            self.discover_jobs_active += 1;
                            self.discover_handles.push(handle)
                        }
                        Err(e) => self.show_message(
                            lsp_types::MessageType::Error,
                            format!("Failed to spawn project discovery command: {e:#}"),
                            false,
                        ),
                    }
                }
            }
            Task::FetchBuildData(progress) => {
                let (state, msg) = match progress {
                    BuildDataProgress::Begin => (Some(Progress::Begin), None),
                    BuildDataProgress::Report(msg) => (Some(Progress::Report), Some(msg)),
                    BuildDataProgress::End((workspaces, build_scripts)) => {
                        let resp = FetchBuildDataResponse { workspaces, build_scripts };
                        self.fetch_build_data_queue.op_completed(resp);

                        if let Err(e) = self.fetch_build_data_error() {
                            error!("FetchBuildDataError: {e}");
                        }

                        if self.wants_to_switch.is_none() {
                            self.wants_to_switch = Some("fetched build data".to_owned());
                        }
                        (Some(Progress::End), None)
                    }
                };

                if let Some(state) = state {
                    self.report_progress("Building compile-time-deps", state, msg, None, None);
                }
            }
            Task::LoadProcMacros(progress) => {
                let (state, msg) = match progress {
                    ProcMacroProgress::Begin => (Some(Progress::Begin), None),
                    ProcMacroProgress::Report(msg) => (Some(Progress::Report), Some(msg)),
                    ProcMacroProgress::End(change) => {
                        self.fetch_proc_macros_queue.op_completed(true);
                        cancellation_time = Some(self.analysis_host.apply_change(change));
                        // FIXME This feels a bit off, this should go through similar machinery as build scripts?
                        _ = self.finish_loading_crate_graph();
                        (Some(Progress::End), None)
                    }
                };

                if let Some(state) = state {
                    self.report_progress("Loading proc-macros", state, msg, None, None);
                }
            }
            Task::BuildDepsHaveChanged => self.build_deps_changed = true,
            Task::DiscoverTest(tests) => {
                self.send_notification::<lsp_ext::DiscoveredTestsNotification>(tests);
            }
        }
        cancellation_time
    }

    fn handle_vfs_msg(
        &mut self,
        message: vfs::loader::Message,
        last_progress_report: &mut Option<(String, f64)>,
    ) {
        let _p = tracing::info_span!("GlobalState::handle_vfs_msg").entered();
        let is_changed = matches!(message, vfs::loader::Message::Changed { .. });
        match message {
            vfs::loader::Message::Changed { files } | vfs::loader::Message::Loaded { files } => {
                let _p = tracing::info_span!("GlobalState::handle_vfs_msg{changed/load}").entered();
                self.debounce_workspace_fetch();
                let vfs = &mut self.vfs.write().0;
                for (path, contents) in files {
                    if matches!(path.name_and_extension(), Some(("minicore", Some("rs")))) {
                        // Not a lot of bad can happen from mistakenly identifying `minicore`, so proceed with that.
                        self.minicore.minicore_text = contents
                            .as_ref()
                            .and_then(|contents| str::from_utf8(contents).ok())
                            .map(triomphe::Arc::from);
                    }

                    let path = VfsPath::from(path);
                    // If the file is in mem docs, it's managed by the client via
                    // notifications so only set it if it's not in there. Library files are
                    // exempt from that authority as they are considered immutable, for
                    // them disk is always the source of truth.
                    let is_library = self.source_root_config.path_is_library(&path);
                    let client_is_authoritative = !is_library && self.mem_docs.contains(&path);
                    if !client_is_authoritative
                        && (is_changed || is_library || vfs.file_id(&path).is_none())
                    {
                        vfs.set_file_contents(path, contents);
                    }
                }
            }
            vfs::loader::Message::Progress { n_total, n_done, dir, config_version } => {
                let _p = span!(Level::INFO, "GlobalState::handle_vfs_msg/progress").entered();
                stdx::always!(config_version <= self.vfs_config_version);

                let (n_done, state) = match n_done {
                    LoadingProgress::Started => {
                        self.vfs_span =
                            Some(span!(Level::INFO, "vfs_load", total = n_total).entered());
                        (0, Progress::Begin)
                    }
                    LoadingProgress::Progress(n_done) => (n_done.min(n_total), Progress::Report),
                    LoadingProgress::Finished => {
                        self.vfs_span = None;
                        (n_total, Progress::End)
                    }
                };

                self.vfs_progress_config_version = config_version;
                self.vfs_done = state == Progress::End;

                let mut message = format!("{n_done}/{n_total}");
                if let Some(dir) = dir {
                    message += &format!(
                        ": {}",
                        match dir.strip_prefix(self.config.workspace_root_for(&dir)) {
                            Some(relative_path) => relative_path.as_utf8_path(),
                            None => dir.as_ref(),
                        }
                    );
                }

                match state {
                    Progress::Begin => self.report_progress(
                        "Roots Scanned",
                        state,
                        Some(message),
                        Some(Progress::fraction(n_done, n_total)),
                        None,
                    ),
                    // Don't send too many notifications while batching, sending progress reports
                    // serializes notifications on the mainthread at the moment which slows us down
                    Progress::Report => {
                        if last_progress_report.is_none() {
                            self.report_progress(
                                "Roots Scanned",
                                state,
                                Some(message.clone()),
                                Some(Progress::fraction(n_done, n_total)),
                                None,
                            );
                        }

                        *last_progress_report =
                            Some((message, Progress::fraction(n_done, n_total)));
                    }
                    Progress::End => {
                        last_progress_report.take();
                        self.report_progress(
                            "Roots Scanned",
                            state,
                            Some(message),
                            Some(Progress::fraction(n_done, n_total)),
                            None,
                        )
                    }
                }
            }
        }
    }

    fn handle_deferred_task(&mut self, task: DeferredTask) {
        match task {
            DeferredTask::CheckIfIndexed(uri) => {
                let snap = self.snapshot();

                self.task_pool.handle.spawn_with_sender(ThreadIntent::Worker, move |sender| {
                    let _p = tracing::info_span!("GlobalState::check_if_indexed").entered();
                    tracing::debug!(?uri, "handling uri");
                    let Some(id) = from_proto::file_id(&snap, &uri).expect("unable to get FileId")
                    else {
                        return;
                    };
                    if let Ok(crates) = &snap.analysis.crates_for(id) {
                        if crates.is_empty() {
                            if snap.config.discover_workspace_config().is_some() {
                                let path =
                                    from_proto::abs_path(&uri).expect("Unable to get AbsPath");
                                let arg = DiscoverProjectParam::Path(path);
                                sender.send(Task::DiscoverLinkedProjects(arg)).unwrap();
                            }
                        } else {
                            tracing::debug!(?uri, "is indexed");
                        }
                    }
                });
            }
            DeferredTask::CheckProcMacroSources(modified_rust_files) => {
                let analysis = AssertUnwindSafe(self.snapshot().analysis);
                self.task_pool.handle.spawn_with_sender(stdx::thread::ThreadIntent::Worker, {
                    move |sender| {
                        if modified_rust_files.into_iter().any(|file_id| {
                            // FIXME: Check whether these files could be build script related
                            match analysis.crates_for(file_id) {
                                Ok(crates) => crates.iter().any(|&krate| {
                                    analysis.is_proc_macro_crate(krate).is_ok_and(|it| it)
                                }),
                                _ => false,
                            }
                        }) {
                            sender.send(Task::BuildDepsHaveChanged).unwrap();
                        }
                    }
                });
            }
        }
    }

    fn handle_discover_msg(&mut self, message: DiscoverProjectMessage) {
        let title = self
            .config
            .discover_workspace_config()
            .map(|cfg| cfg.progress_label.clone())
            .expect("No title could be found; this is a bug");
        match message {
            DiscoverProjectMessage::Finished { project, buildfile } => {
                self.discover_jobs_active = self.discover_jobs_active.saturating_sub(1);
                if self.discover_jobs_active == 0 {
                    self.report_progress(&title, Progress::End, None, None, None);
                }

                let mut config = Config::clone(&*self.config);
                config.add_discovered_project_from_command(project, buildfile);
                self.update_configuration(config);
            }
            DiscoverProjectMessage::Progress { message } => {
                if self.discover_jobs_active > 0 {
                    self.report_progress(&title, Progress::Report, Some(message), None, None)
                }
            }
            DiscoverProjectMessage::Error { error, source } => {
                let message = format!("Project discovery failed: {error}");
                self.show_and_log_error(message.clone(), source);

                self.discover_jobs_active = self.discover_jobs_active.saturating_sub(1);
                if self.discover_jobs_active == 0 {
                    self.report_progress(&title, Progress::End, Some(message), None, None)
                }
            }
        }
    }

    /// Drop any discover command processes that have exited, due to
    /// finishing or erroring.
    fn cleanup_discover_handles(&mut self) {
        let mut active_handles = vec![];

        for mut discover_handle in self.discover_handles.drain(..) {
            if !discover_handle.handle.has_exited() {
                active_handles.push(discover_handle);
            }
        }
        self.discover_handles = active_handles;
    }

    fn handle_cargo_test_msg(&mut self, message: CargoTestMessage) {
        match message.output {
            CargoTestOutput::Test { name, state } => {
                let state = match state {
                    TestState::Started => lsp_ext::TestState::Started,
                    TestState::Ignored => lsp_ext::TestState::Skipped,
                    TestState::Ok => lsp_ext::TestState::Passed,
                    TestState::Failed { stdout } => lsp_ext::TestState::Failed { message: stdout },
                };

                // The notification requires the namespace form (with underscores) of the target
                let test_id = format!("{}::{name}", message.target.target.replace('-', "_"));

                self.send_notification::<lsp_ext::ChangeTestStateNotification>(
                    lsp_ext::ChangeTestStateParams { test_id, state },
                );
            }
            CargoTestOutput::Suite => (),
            CargoTestOutput::Finished => {
                self.test_run_remaining_jobs = self.test_run_remaining_jobs.saturating_sub(1);
                if self.test_run_remaining_jobs == 0 {
                    self.send_notification::<lsp_ext::EndRunTestNotification>(());
                    self.test_run_session = None;
                }
            }
            CargoTestOutput::Custom { text } => {
                self.send_notification::<lsp_ext::AppendOutputToRunTestNotification>(text);
            }
        }
    }

    fn schedule_ownership_model_artifacts(
        &mut self,
        id: usize,
        package_id: Option<flycheck::PackageSpecifier>,
        paths: Vec<std::path::PathBuf>,
    ) {
        if paths.is_empty() {
            return;
        }
        self.ownership_artifact_sequence = self.ownership_artifact_sequence.wrapping_add(1);
        let sequence = self.ownership_artifact_sequence;
        let snap = self.snapshot();
        self.task_pool.handle.spawn_with_sender(ThreadIntent::Worker, move |sender| {
            let started = Instant::now();
            let files = prepare_ownership_model_artifacts(&snap, paths);
            let preparation_time = started.elapsed();
            // Install one source file per main-loop turn. The persistent caches make each commit
            // cheap, but committing an entire multi-crate workspace in one event can still exceed
            // a frame budget because every file also publishes a model-changed notification.
            for file in files {
                if sender
                    .send(Task::OwnershipArtifacts(PreparedOwnershipBatch {
                        id,
                        package_id: package_id.clone(),
                        sequence,
                        files: vec![file],
                        preparation_time,
                    }))
                    .is_err()
                {
                    break;
                }
            }
        });
    }

    fn commit_ownership_model_artifacts(&mut self, batch: PreparedOwnershipBatch) {
        let commit_started = Instant::now();
        for file in batch.files {
            if self
                .ownership_file_sequences
                .get(&file.file_id)
                .is_some_and(|latest| *latest > batch.sequence)
            {
                // These payloads contain nested MIR timelines and can be large. Destroy stale
                // duplicate artifacts on a worker so deallocation cannot pause LSP dispatch.
                self.task_pool
                    .handle
                    .spawn_with_sender(ThreadIntent::Worker, move |_sender| drop(file));
                continue;
            }
            if self.ownership_file_hashes.get(&file.file_id) != Some(&file.vfs_hash) {
                self.task_pool
                    .handle
                    .spawn_with_sender(ThreadIntent::Worker, move |_sender| drop(file));
                continue;
            }

            let mut retired = self.diagnostics.replace_exact_ownership_events(
                batch.id,
                &batch.package_id,
                file.file_id,
                file.events,
            );
            if let Some(model) = file.model {
                if let Some(model) =
                    self.diagnostics.set_ownership_tutorial_model(file.file_id, model)
                {
                    retired.tutorial_models.push(model);
                }
            }
            self.ownership_file_sequences.insert(file.file_id, batch.sequence);
            self.send_notification::<lsp_ext::OwnershipModelChangedNotification>(
                lsp_ext::OwnershipModelChangedParams {
                    uri: file.uri,
                    schema_version: file.schema_version,
                    status: "ready".to_owned(),
                    source_hash: file.source_hash,
                },
            );
            if !retired.is_empty() {
                self.task_pool
                    .handle
                    .spawn_with_sender(ThreadIntent::Worker, move |_sender| drop(retired));
            }
        }
        tracing::debug!(
            preparation_time = ?batch.preparation_time,
            commit_time = ?commit_started.elapsed(),
            "committed compiler ownership artifacts"
        );
    }

    fn handle_flycheck_msg(&mut self, message: FlycheckMessage, cargo_finished: &mut bool) {
        match message {
            FlycheckMessage::AddDiagnostic {
                id,
                generation,
                workspace_root,
                diagnostic,
                package_id,
            } => {
                let ownership_model_transport = diagnostic
                    .code
                    .as_ref()
                    .is_some_and(|code| code.code == "borrowck_ownership_model");
                let ownership_transport = diagnostic.code.as_ref().is_some_and(|code| {
                    code.code.starts_with("borrowck_ownership_")
                        && code.code != "borrowck_ownership_model"
                });
                let invalid_use = self.config.ownership_enabled(None)
                    && diagnostic.code.as_ref().is_some_and(|code| code.code == "E0382");
                let invalid_use_name = invalid_use
                    .then(|| diagnostic.message.rsplit('`').nth(1).unwrap_or("value").to_owned());
                let ownership_diagnostic = diagnostic.code.as_ref().and_then(|code| {
                    is_teachable_rust_diagnostic(&code.code)
                        .then(|| (code.code.clone(), diagnostic.message.clone()))
                });
                let ownership_payload = ownership_transport
                    .then(|| serde_json::from_str::<OwnershipEventPayload>(&diagnostic.message));
                if ownership_model_transport {
                    let pointer =
                        match serde_json::from_str::<OwnershipModelPointer>(&diagnostic.message) {
                            Ok(pointer) if matches!(pointer.version, 2 | 3 | 4) => pointer,
                            Ok(pointer) => {
                                tracing::warn!(
                                    version = pointer.version,
                                    "ignored unknown ownership model"
                                );
                                return;
                            }
                            Err(error) => {
                                tracing::warn!(%error, "ignored malformed ownership model pointer");
                                return;
                            }
                        };
                    self.pending_ownership_artifact_paths.entry(id).or_default().push(pointer.path);
                    return;
                }
                let snap = self.snapshot();
                let diagnostics = crate::diagnostics::flycheck_to_proto::map_rust_diagnostic_to_lsp(
                    &self.config.diagnostics_map(None),
                    diagnostic,
                    &workspace_root,
                    &snap,
                );
                if ownership_transport {
                    let Ok(payload) = ownership_payload.expect("ownership payload was requested")
                    else {
                        tracing::warn!("ignored malformed rustc ownership event");
                        return;
                    };
                    if payload.version != 1 {
                        tracing::warn!(
                            version = payload.version,
                            "ignored unknown ownership event"
                        );
                        return;
                    }
                    for diag in diagnostics {
                        let Ok(Some(file_id)) = url_to_file_id(&self.vfs.read().0, &diag.url)
                        else {
                            continue;
                        };
                        let Ok(text) = snap.analysis.file_text(file_id) else { continue };
                        if stable_source_hash(&text) != payload.source_hash {
                            continue;
                        }
                        let Ok(binding_start) = TextSize::try_from(payload.binding_byte_start)
                        else {
                            continue;
                        };
                        let Ok(binding_len) = TextSize::try_from(payload.name.len()) else {
                            continue;
                        };
                        if binding_start + binding_len > TextSize::of(&*text) {
                            continue;
                        }
                        let Ok(line_index) = snap.file_line_index(file_id) else { continue };
                        let binding_range = lsp_types::Range::new(
                            to_proto::position(&line_index, binding_start),
                            to_proto::position(&line_index, binding_start + binding_len),
                        );
                        self.diagnostics.add_ownership_event(
                            id,
                            &package_id,
                            file_id,
                            OwnershipEvent {
                                event_id: format!(
                                    "legacy-{}-{}",
                                    payload.binding_byte_start, diag.diagnostic.range.start.line
                                ),
                                body_id: 0,
                                basic_block: 0,
                                statement_index: 0,
                                kind: payload.kind,
                                state: ownership_state_for_kind(payload.kind),
                                range: diag.diagnostic.range,
                                binding_range,
                                name: payload.name.clone(),
                                place: payload.name.clone(),
                                loan_id: None,
                                exact: true,
                                detail: payload.detail.clone(),
                                destination: None,
                            },
                        );
                    }
                    return;
                }
                for diag in diagnostics {
                    match url_to_file_id(&self.vfs.read().0, &diag.url) {
                        Ok(Some(file_id)) => {
                            if let Some((code, message)) = &ownership_diagnostic
                                && should_record_invalid_use(diag.diagnostic.severity)
                            {
                                let related = diag
                                    .diagnostic
                                    .related_information
                                    .as_deref()
                                    .unwrap_or_default()
                                    .iter()
                                    .filter(|related| related.location.uri == diag.url)
                                    .map(|related| crate::diagnostics::OwnershipDiagnosticRelated {
                                        message: related.message.clone(),
                                        range: related.location.range,
                                    })
                                    .collect();
                                self.diagnostics.add_ownership_diagnostic(
                                    id,
                                    &package_id,
                                    file_id,
                                    OwnershipDiagnostic {
                                        code: code.clone(),
                                        message: message.clone(),
                                        range: diag.diagnostic.range,
                                        related,
                                    },
                                );
                            }
                            // `map_rust_diagnostic_to_lsp` also materializes rustc's secondary
                            // spans (the declaration, move site, and help) as hint diagnostics.
                            // Only the error-level primary span is the rejected post-move use.
                            if let Some(name) = &invalid_use_name
                                && should_record_invalid_use(diag.diagnostic.severity)
                            {
                                self.diagnostics.add_ownership_event(
                                    id,
                                    &package_id,
                                    file_id,
                                    OwnershipEvent {
                                        event_id: format!("invalid-use-{name}-{}", diag.diagnostic.range.start.line),
                                        body_id: 0,
                                        basic_block: 0,
                                        statement_index: 0,
                                        kind: crate::diagnostics::OwnershipEventKind::InvalidUse,
                                        state: OwnershipState::Moved,
                                        range: diag.diagnostic.range,
                                        binding_range: diag.diagnostic.range,
                                        name: name.clone(),
                                        place: name.clone(),
                                        loan_id: None,
                                        exact: true,
                                        detail: Some(
                                            "rustc rejected this use because the value was already moved"
                                                .to_owned(),
                                        ),
                                        destination: None,
                                    },
                                );
                            }
                            self.diagnostics.add_check_diagnostic(
                                id,
                                generation,
                                &package_id,
                                file_id,
                                diag.diagnostic,
                                diag.fix,
                            )
                        }
                        Ok(None) => {}
                        Err(err) => {
                            error!(
                                "flycheck {id}: File with cargo diagnostic not found in VFS: {}",
                                err
                            );
                        }
                    };
                }
            }
            FlycheckMessage::OwnershipArtifacts { id, paths } => {
                self.pending_ownership_artifact_paths.entry(id).or_default().extend(paths);
            }
            FlycheckMessage::ClearDiagnostics {
                id,
                kind: ClearDiagnosticsKind::All(ClearScope::Workspace),
            } => self.diagnostics.clear_check(id),
            FlycheckMessage::ClearDiagnostics {
                id,
                kind: ClearDiagnosticsKind::All(ClearScope::Package(package_id)),
            } => self.diagnostics.clear_check_for_package(id, package_id),
            FlycheckMessage::ClearDiagnostics {
                id,
                kind: ClearDiagnosticsKind::OlderThan(generation, ClearScope::Workspace),
            } => self.diagnostics.clear_check_older_than(id, generation),
            FlycheckMessage::ClearDiagnostics {
                id,
                kind: ClearDiagnosticsKind::OlderThan(generation, ClearScope::Package(package_id)),
            } => self.diagnostics.clear_check_older_than_for_package(id, package_id, generation),
            FlycheckMessage::Progress { id, progress } => {
                let mut ownership_artifacts_to_schedule = None;
                let format_with_id = |user_facing_command: String| {
                    // When we're running multiple flychecks, we have to include a disambiguator in
                    // the title, or the editor complains. Note that this is a user-facing string.
                    if self.flycheck.len() == 1 {
                        user_facing_command
                    } else {
                        format!("{user_facing_command} (#{})", id + 1)
                    }
                };

                self.flycheck_formatted_commands
                    .resize_with(self.flycheck.len().max(id + 1), || {
                        format_with_id(self.config.flycheck(None).to_string())
                    });

                let (state, message) = match progress {
                    flycheck::Progress::DidStart { user_facing_command } => {
                        self.pending_ownership_artifact_paths.remove(&id);
                        self.flycheck_formatted_commands[id] = format_with_id(user_facing_command);
                        (Progress::Begin, None)
                    }
                    flycheck::Progress::DidCheckCrate(target) => (Progress::Report, Some(target)),
                    flycheck::Progress::DidCancel => {
                        self.last_flycheck_error = None;
                        self.pending_ownership_artifact_paths.remove(&id);
                        *cargo_finished = true;
                        (Progress::End, None)
                    }
                    flycheck::Progress::DidFailToRestart(err) => {
                        self.pending_ownership_artifact_paths.remove(&id);
                        self.last_flycheck_error =
                            Some(format!("cargo check failed to start: {err}"));
                        return;
                    }
                    flycheck::Progress::DidFinish(result) => {
                        ownership_artifacts_to_schedule =
                            self.pending_ownership_artifact_paths.remove(&id);
                        self.last_flycheck_error =
                            result.err().map(|err| format!("cargo check failed to start: {err}"));
                        *cargo_finished = true;
                        (Progress::End, None)
                    }
                };

                // Clone because we &mut self for report_progress
                let title = self.flycheck_formatted_commands[id].clone();
                self.report_progress(
                    &title,
                    state,
                    message,
                    None,
                    Some(format!("rust-analyzer/flycheck/{id}")),
                );
                if let Some(paths) = ownership_artifacts_to_schedule {
                    self.schedule_ownership_model_artifacts(id, None, paths);
                }
            }
        }
    }

    /// Registers and handles a request. This should only be called once per incoming request.
    fn on_new_request(&mut self, request_received: Instant, req: Request) {
        let _p =
            span!(Level::INFO, "GlobalState::on_new_request", req.method = ?req.method).entered();
        self.register_request(&req, request_received);
        self.on_request(req);
    }

    /// Handles a request.
    fn on_request(&mut self, req: Request) {
        let mut dispatcher = RequestDispatcher { req: Some(req), global_state: self };
        dispatcher.on_sync_mut::<lsp_types::ShutdownRequest>(|s, ()| {
            s.shutdown_requested = true;
            s.proc_macro_clients =
                std::iter::repeat_with(|| None).take(s.proc_macro_clients.len()).collect();
            s.flycheck.iter().for_each(|handle| handle.cancel());
            s.discover_handles.clear();
            Ok(())
        });

        match &mut dispatcher {
            RequestDispatcher { req: Some(req), global_state: this } if this.shutdown_requested => {
                this.respond(lsp_server::Response::new_err(
                    req.id.clone(),
                    lsp_server::ErrorCode::InvalidRequest as i32,
                    "Shutdown already requested.".to_owned(),
                ));
                return;
            }
            _ => (),
        }

        use crate::handlers::request as handlers;

        const RETRY: bool = true;
        const NO_RETRY: bool = false;

        #[rustfmt::skip]
        dispatcher
            // Request handlers that must run on the main thread
            // because they mutate GlobalState:
            .on_sync_mut::<lsp_ext::ReloadWorkspaceRequest>(handlers::handle_workspace_reload)
            .on_sync_mut::<lsp_ext::RebuildProcMacrosRequest>(handlers::handle_proc_macros_rebuild)
            .on_sync_mut::<lsp_ext::MemoryUsageRequest>(handlers::handle_memory_usage)
            .on_sync_mut::<lsp_ext::RunTestRequest>(handlers::handle_run_test)
            .on_sync_mut::<lsp_ext::OwnershipValidateRepairRequest>(handlers::handle_ownership_validate_repair)
            // Request handlers which are related to the user typing
            // are run on the main thread to reduce latency:
            .on_sync::<lsp_ext::JoinLinesRequest>(handlers::handle_join_lines)
            .on_sync::<lsp_ext::OnEnterRequest>(handlers::handle_on_enter)
            .on_sync::<lsp_types::SelectionRangeRequest>(handlers::handle_selection_range)
            .on_sync::<lsp_ext::MatchingBraceRequest>(handlers::handle_matching_brace)
            .on_sync::<lsp_ext::DocumentOnTypeFormattingRequest>(handlers::handle_on_type_formatting)
            // Formatting should be done immediately as the editor might wait on it, but we can't
            // put it on the main thread as we do not want the main thread to block on rustfmt.
            // So we have an extra thread just for formatting requests to make sure it gets handled
            // as fast as possible.
            .on_fmt_thread::<lsp_types::DocumentFormattingRequest>(handlers::handle_formatting)
            .on_fmt_thread::<lsp_types::DocumentRangeFormattingRequest>(handlers::handle_range_formatting)
            // We can’t run latency-sensitive request handlers which do semantic
            // analysis on the main thread because that would block other
            // requests. Instead, we run these request handlers on higher priority
            // threads in the threadpool.
            // FIXME: Retrying can make the result of this stale?
            .on_latency_sensitive::<RETRY, lsp_types::CompletionRequest>(handlers::handle_completion)
            // FIXME: Retrying can make the result of this stale
            .on_latency_sensitive::<RETRY, lsp_types::CompletionResolveRequest>(handlers::handle_completion_resolve)
            .on_latency_sensitive::<RETRY, lsp_types::SemanticTokensRequest>(handlers::handle_semantic_tokens_full)
            .on_latency_sensitive::<RETRY, lsp_types::SemanticTokensDeltaRequest>(handlers::handle_semantic_tokens_full_delta)
            .on_latency_sensitive::<NO_RETRY, lsp_types::SemanticTokensRangeRequest>(handlers::handle_semantic_tokens_range)
            // FIXME: Some of these NO_RETRY could be retries if the file they are interested didn't change.
            // All other request handlers
            .on_with_vfs_default::<lsp_types::DocumentDiagnosticRequest>(handlers::handle_document_diagnostics, empty_diagnostic_report, || lsp_server::ResponseError {
                code: lsp_server::ErrorCode::ServerCancelled as i32,
                message: "server cancelled the request".to_owned(),
                data: serde_json::to_value(lsp_types::DiagnosticServerCancellationData {
                    retrigger_request: true
                }).ok(),
            })
            .on::<RETRY, lsp_types::DocumentSymbolRequest>(handlers::handle_document_symbol)
            .on::<RETRY, lsp_types::FoldingRangeRequest>(handlers::handle_folding_range)
            .on::<NO_RETRY, lsp_types::SignatureHelpRequest>(handlers::handle_signature_help)
            .on::<RETRY, lsp_types::WillRenameFilesRequest>(handlers::handle_will_rename_files)
            .on::<NO_RETRY, lsp_types::DefinitionRequest>(handlers::handle_goto_definition)
            .on::<NO_RETRY, lsp_types::DeclarationRequest>(handlers::handle_goto_declaration)
            .on::<NO_RETRY, lsp_types::ImplementationRequest>(handlers::handle_goto_implementation)
            .on::<NO_RETRY, lsp_types::TypeDefinitionRequest>(handlers::handle_goto_type_definition)
            .on::<NO_RETRY, lsp_types::InlayHintRequest>(handlers::handle_inlay_hints)
            .on_identity::<NO_RETRY, lsp_types::InlayHintResolveRequest, _>(handlers::handle_inlay_hints_resolve)
            .on::<NO_RETRY, lsp_types::CodeLensRequest>(handlers::handle_code_lens)
            .on_identity::<NO_RETRY, lsp_types::CodeLensResolveRequest, _>(handlers::handle_code_lens_resolve)
            .on::<NO_RETRY, lsp_types::PrepareRenameRequest>(handlers::handle_prepare_rename)
            .on::<NO_RETRY, lsp_types::RenameRequest>(handlers::handle_rename)
            .on::<NO_RETRY, lsp_types::ReferencesRequest>(handlers::handle_references)
            .on::<NO_RETRY, lsp_types::DocumentHighlightRequest>(handlers::handle_document_highlight)
            .on::<NO_RETRY, lsp_types::CallHierarchyPrepareRequest>(handlers::handle_call_hierarchy_prepare)
            .on::<NO_RETRY, lsp_types::CallHierarchyIncomingCallsRequest>(handlers::handle_call_hierarchy_incoming)
            .on::<NO_RETRY, lsp_types::CallHierarchyOutgoingCallsRequest>(handlers::handle_call_hierarchy_outgoing)
            // All other request handlers (lsp extension)
            .on::<RETRY, lsp_ext::FetchDependencyListRequest>(handlers::fetch_dependency_list)
            .on::<RETRY, lsp_ext::AnalyzerStatusRequest>(handlers::handle_analyzer_status)
            .on::<RETRY, lsp_ext::ViewFileTextRequest>(handlers::handle_view_file_text)
            .on::<RETRY, lsp_ext::ViewCrateGraphRequest>(handlers::handle_view_crate_graph)
            .on::<RETRY, lsp_ext::ViewItemTreeRequest>(handlers::handle_view_item_tree)
            .on::<RETRY, lsp_ext::DiscoverTestRequest>(handlers::handle_discover_test)
            .on::<RETRY, lsp_ext::WorkspaceSymbolRequest>(handlers::handle_workspace_symbol)
            .on::<NO_RETRY, lsp_ext::SsrRequest>(handlers::handle_ssr)
            .on::<NO_RETRY, lsp_ext::ViewRecursiveMemoryLayoutRequest>(handlers::handle_view_recursive_memory_layout)
            .on::<NO_RETRY, lsp_ext::ViewSyntaxTreeRequest>(handlers::handle_view_syntax_tree)
            .on::<NO_RETRY, lsp_ext::ViewHirRequest>(handlers::handle_view_hir)
            .on::<NO_RETRY, lsp_ext::ViewMirRequest>(handlers::handle_view_mir)
            .on::<NO_RETRY, lsp_ext::InterpretFunctionRequest>(handlers::handle_interpret_function)
            .on::<NO_RETRY, lsp_ext::ExpandMacroRequest>(handlers::handle_expand_macro)
            .on::<NO_RETRY, lsp_ext::ParentModuleRequest>(handlers::handle_parent_module)
            .on::<NO_RETRY, lsp_ext::ChildModulesRequest>(handlers::handle_child_modules)
            .on::<NO_RETRY, lsp_ext::RunnablesRequest>(handlers::handle_runnables)
            .on::<NO_RETRY, lsp_ext::RelatedTestsRequest>(handlers::handle_related_tests)
            .on::<NO_RETRY, lsp_ext::CodeActionRequest>(handlers::handle_code_action)
            .on_identity::<RETRY, lsp_ext::CodeActionResolveRequest, _>(handlers::handle_code_action_resolve)
            .on::<NO_RETRY, lsp_ext::HoverRequest>(handlers::handle_hover)
            .on::<NO_RETRY, lsp_ext::OwnershipProblemsRequest>(handlers::handle_ownership_problems)
            .on::<NO_RETRY, lsp_ext::OwnershipRepairRequest>(handlers::handle_ownership_repair)
            .on::<NO_RETRY, lsp_ext::OwnershipModelRequest>(handlers::handle_ownership_model)
            .on::<NO_RETRY, lsp_ext::ExternalDocsRequest>(handlers::handle_open_docs)
            .on::<NO_RETRY, lsp_ext::OpenCargoTomlRequest>(handlers::handle_open_cargo_toml)
            .on::<NO_RETRY, lsp_ext::MoveItemRequest>(handlers::handle_move_item)
            //
            .on::<NO_RETRY, lsp_ext::InternalTestingFetchConfigRequest>(handlers::internal_testing_fetch_config)
            .on::<RETRY, lsp_ext::EvaluatePredicateRequest>(handlers::handle_evaluate_predicate)
            .on::<RETRY, lsp_ext::GetFailedObligationsRequest>(handlers::get_failed_obligations)
            .finish();
    }

    /// Handles an incoming notification.
    fn on_notification(&mut self, not: Notification) {
        let _p =
            span!(Level::INFO, "GlobalState::on_notification", not.method = ?not.method).entered();
        use crate::handlers::notification as handlers;

        NotificationDispatcher { not: Some(not), global_state: self }
            .on_sync_mut::<lsp_types::CancelNotification>(handlers::handle_cancel)
            .on_sync_mut::<lsp_types::WorkDoneProgressCancelNotification>(
                handlers::handle_work_done_progress_cancel,
            )
            .on_sync_mut::<lsp_types::DidOpenTextDocumentNotification>(
                handlers::handle_did_open_text_document,
            )
            .on_sync_mut::<lsp_types::DidChangeTextDocumentNotification>(
                handlers::handle_did_change_text_document,
            )
            .on_sync_mut::<lsp_types::DidCloseTextDocumentNotification>(
                handlers::handle_did_close_text_document,
            )
            .on_sync_mut::<lsp_types::DidSaveTextDocumentNotification>(
                handlers::handle_did_save_text_document,
            )
            .on_sync_mut::<lsp_types::DidChangeConfigurationNotification>(
                handlers::handle_did_change_configuration,
            )
            .on_sync_mut::<lsp_types::DidChangeWorkspaceFoldersNotification>(
                handlers::handle_did_change_workspace_folders,
            )
            .on_sync_mut::<lsp_types::DidChangeWatchedFilesNotification>(
                handlers::handle_did_change_watched_files,
            )
            .on_sync_mut::<lsp_ext::CancelFlycheckNotification>(handlers::handle_cancel_flycheck)
            .on_sync_mut::<lsp_ext::ClearFlycheckNotification>(handlers::handle_clear_flycheck)
            .on_sync_mut::<lsp_ext::RunFlycheckNotification>(handlers::handle_run_flycheck)
            .on_sync_mut::<lsp_ext::AbortRunTestNotification>(handlers::handle_abort_run_test)
            .finish();
    }
}

fn prepare_ownership_model_artifacts(
    snap: &crate::global_state::GlobalStateSnapshot,
    paths: Vec<std::path::PathBuf>,
) -> Vec<PreparedOwnershipFile> {
    // A cargo workspace commonly produces one artifact per target (for example a binary and its
    // test harness). Keep only the newest model for each source file while replaying the sorted
    // cache, so the main loop receives one small commit instead of dozens of duplicate updates.
    let mut prepared = FxHashMap::<FileId, PreparedOwnershipFile>::default();
    for path in paths {
        let artifact = match read_ownership_model_artifact(&path) {
            Ok(artifact) if matches!(artifact.schema_version, 2 | 3 | 4 | 5 | 6) => artifact,
            Ok(artifact) => {
                tracing::warn!(
                    version = artifact.schema_version,
                    path = %path.display(),
                    "ignored unknown ownership model artifact"
                );
                continue;
            }
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "failed to read ownership model");
                continue;
            }
        };

        for source in &artifact.sources {
            let Ok(uri) = lsp_types::Uri::from_file_path(&source.path) else { continue };
            let Ok(Some(file_id)) = snap.url_to_file_id(&uri) else { continue };
            let Ok(text) = snap.analysis.file_text(file_id) else { continue };
            let Some(vfs_hash) = snap.file_content_hash(file_id) else { continue };
            let source_hash = stable_source_hash(&text);
            let artifact_source_hash = source
                .source
                .as_deref()
                .map(stable_source_hash)
                .unwrap_or_else(|| source.source_hash.clone());
            if source_hash != artifact_source_hash {
                continue;
            }
            let source_text = source.source.as_deref().unwrap_or(&text);
            let Ok(line_index) = snap.file_line_index(file_id) else { continue };

            let events = artifact
                .ownership_events
                .iter()
                .filter(|event| event.path == source.path)
                .filter_map(|event| {
                    let (event_start, event_end) = ownership_model_event_range(
                        source_text,
                        event.byte_start,
                        event.byte_end,
                        &event.binding.name,
                    )?;
                    let binding_end =
                        event.binding.byte_start.checked_add(event.binding.name.len())?;
                    if binding_end > source_text.len()
                        || !source_text.is_char_boundary(event.binding.byte_start)
                        || !source_text.is_char_boundary(binding_end)
                    {
                        return None;
                    }
                    let event_start = TextSize::try_from(event_start).ok()?;
                    let event_end = TextSize::try_from(event_end).ok()?;
                    let binding_start = TextSize::try_from(event.binding.byte_start).ok()?;
                    let binding_end = TextSize::try_from(binding_end).ok()?;
                    Some(OwnershipEvent {
                        event_id: event.event_id.clone(),
                        body_id: event.body_id,
                        basic_block: event.basic_block,
                        statement_index: event.statement_index,
                        kind: event.kind,
                        state: event.state,
                        range: lsp_types::Range::new(
                            to_proto::position(&line_index, event_start),
                            to_proto::position(&line_index, event_end),
                        ),
                        binding_range: lsp_types::Range::new(
                            to_proto::position(&line_index, binding_start),
                            to_proto::position(&line_index, binding_end),
                        ),
                        name: event.binding.name.clone(),
                        place: event.place.clone(),
                        loan_id: event.loan_id,
                        exact: true,
                        detail: event.detail.clone(),
                        destination: event.destination.as_ref().map(|destination| {
                            let range = destination.span.as_ref().and_then(|span| {
                                if span.path != source.path
                                    || span.byte_start > span.byte_end
                                    || span.byte_end > source_text.len()
                                {
                                    return None;
                                }
                                let start = TextSize::try_from(span.byte_start).ok()?;
                                let end = TextSize::try_from(span.byte_end).ok()?;
                                Some(lsp_types::Range::new(
                                    to_proto::position(&line_index, start),
                                    to_proto::position(&line_index, end),
                                ))
                            });
                            OwnershipDestination {
                                kind: destination.kind.clone(),
                                label: destination.label.clone(),
                                place: destination.place.clone(),
                                range,
                            }
                        }),
                    })
                })
                .collect();
            let model =
                ownership_tutorial_model_for_source(&line_index, source, source_text, &artifact);
            let model = (!model.bodies.is_empty()
                || !model.bindings.is_empty()
                || !model.loans.is_empty()
                || !model.memory_graph.nodes.is_empty())
            .then_some(model);
            prepared.insert(
                file_id,
                PreparedOwnershipFile {
                    uri,
                    file_id,
                    vfs_hash,
                    source_hash,
                    schema_version: artifact.schema_version,
                    events,
                    model,
                },
            );
        }
    }
    prepared.into_values().collect()
}

fn ownership_tutorial_model_for_source(
    line_index: &crate::line_index::LineIndex,
    source: &OwnershipModelSource,
    source_text: &str,
    artifact: &OwnershipModelArtifact,
) -> OwnershipTutorialModel {
    let range = |span: &OwnershipModelSpan| {
        ownership_model_span_range(line_index, source, source_text, span)
    };
    let point = |point: &OwnershipModelLoanPoint| {
        Some(OwnershipTutorialLoanPoint {
            basic_block: point.basic_block,
            statement_index: point.statement_index,
            range: range(&point.span)?,
        })
    };
    let bodies = artifact
        .ownership_bodies
        .iter()
        .filter_map(|body| {
            Some(OwnershipTutorialBody {
                body_id: body.body_id,
                name: body.name.clone(),
                range: range(&body.span)?,
                blocks: body
                    .blocks
                    .iter()
                    .filter_map(|block| {
                        Some(OwnershipTutorialBlock {
                            basic_block: block.basic_block,
                            range: range(&block.span)?,
                            successors: block.successors.clone(),
                        })
                    })
                    .collect(),
            })
        })
        .collect();
    let bindings = artifact
        .ownership_bindings
        .iter()
        .filter(|binding| binding.binding.path == source.path)
        .filter_map(|binding| {
            let byte_end = binding.binding.byte_start.checked_add(binding.binding.name.len())?;
            Some(OwnershipTutorialBinding {
                body_id: binding.body_id,
                name: binding.binding.name.clone(),
                range: ownership_model_byte_range(
                    line_index,
                    source_text,
                    binding.binding.byte_start,
                    byte_end,
                )?,
                type_name: binding.type_name.clone(),
                size: binding.size,
                align: binding.align,
                memory_layers: binding.memory_layers.clone(),
            })
        })
        .collect();
    let loans = artifact
        .ownership_loans
        .iter()
        .filter(|loan| loan.binding.path == source.path)
        .filter_map(|loan| {
            Some(OwnershipTutorialLoan {
                body_id: loan.body_id,
                loan_id: loan.loan_id,
                kind: loan.kind.clone(),
                name: loan.binding.name.clone(),
                place: loan.place.clone(),
                reserve: point(&loan.reserve)?,
                activation: loan.activation.as_ref().and_then(&point),
                live_points: loan.live_points.iter().filter_map(&point).collect(),
                end_points: loan.end_points.iter().filter_map(&point).collect(),
                truncated: loan.truncated,
            })
        })
        .collect();
    let nodes = artifact
        .memory_graph
        .nodes
        .iter()
        .filter_map(|node| {
            let node_range = match node.span.as_ref() {
                Some(span) => Some(range(span)?),
                None => None,
            };
            Some(OwnershipTutorialMemoryNode { node: node.clone(), range: node_range })
        })
        .collect::<Vec<_>>();
    let node_is_present = |id: &str| nodes.iter().any(|node| node.node.id == id);
    let edges = artifact
        .memory_graph
        .edges
        .iter()
        .filter(|edge| node_is_present(&edge.source) && node_is_present(&edge.target))
        .map(|edge| crate::diagnostics::OwnershipTutorialMemoryEdge {
            range: edge.span.as_ref().and_then(&range),
            edge: edge.clone(),
        })
        .collect();
    let snapshots = artifact
        .memory_graph
        .snapshots
        .iter()
        .filter_map(|snapshot| {
            Some(OwnershipTutorialSnapshot {
                snapshot: snapshot.clone(),
                range: range(&snapshot.span)?,
            })
        })
        .collect();
    let access_paths = artifact
        .memory_graph
        .access_paths
        .iter()
        .filter(|path| node_is_present(&path.node_id))
        .cloned()
        .collect();
    OwnershipTutorialModel {
        schema_version: artifact.schema_version,
        target_triple: artifact.target_triple.clone(),
        bodies,
        bindings,
        loans,
        memory_graph: OwnershipTutorialMemoryGraph {
            nodes,
            edges,
            snapshots,
            access_paths,
            truncated: artifact.memory_graph.truncated,
        },
    }
}

fn ownership_model_span_range(
    line_index: &crate::line_index::LineIndex,
    source: &OwnershipModelSource,
    source_text: &str,
    span: &OwnershipModelSpan,
) -> Option<lsp_types::Range> {
    if span.path != source.path {
        return None;
    }
    ownership_model_byte_range(line_index, source_text, span.byte_start, span.byte_end)
}

fn ownership_model_byte_range(
    line_index: &crate::line_index::LineIndex,
    source: &str,
    byte_start: usize,
    byte_end: usize,
) -> Option<lsp_types::Range> {
    if byte_end > source.len()
        || byte_start > byte_end
        || !source.is_char_boundary(byte_start)
        || !source.is_char_boundary(byte_end)
    {
        return None;
    }
    let start = TextSize::try_from(byte_start).ok()?;
    let end = TextSize::try_from(byte_end).ok()?;
    Some(lsp_types::Range::new(
        to_proto::position(line_index, start),
        to_proto::position(line_index, end),
    ))
}

fn ownership_model_event_range(
    source: &str,
    byte_start: usize,
    byte_end: usize,
    binding_name: &str,
) -> Option<(usize, usize)> {
    if byte_end > source.len()
        || byte_start > byte_end
        || !source.is_char_boundary(byte_start)
        || !source.is_char_boundary(byte_end)
    {
        return None;
    }
    match source[byte_start..byte_end].rfind(binding_name) {
        Some(relative) => {
            let start = byte_start + relative;
            Some((start, start + binding_name.len()))
        }
        None => Some((byte_start, byte_end)),
    }
}

fn ownership_state_for_kind(kind: crate::diagnostics::OwnershipEventKind) -> OwnershipState {
    use crate::diagnostics::OwnershipEventKind;

    match kind {
        OwnershipEventKind::BorrowActivate | OwnershipEventKind::BorrowMutable => {
            OwnershipState::MutablyBorrowed
        }
        OwnershipEventKind::BorrowShared => OwnershipState::SharedBorrowed,
        OwnershipEventKind::Clone | OwnershipEventKind::Copy => OwnershipState::Available,
        OwnershipEventKind::Drop => OwnershipState::Dropped,
        OwnershipEventKind::InvalidUse | OwnershipEventKind::Move => OwnershipState::Moved,
        OwnershipEventKind::PartialMove => OwnershipState::PartiallyMoved,
        OwnershipEventKind::BorrowEnd
        | OwnershipEventKind::LastUse
        | OwnershipEventKind::Reinitialize => OwnershipState::Available,
    }
}

fn should_record_invalid_use(severity: Option<lsp_types::DiagnosticSeverity>) -> bool {
    severity == Some(lsp_types::DiagnosticSeverity::Error)
}

fn is_teachable_rust_diagnostic(code: &str) -> bool {
    matches!(
        code,
        "E0106"
            | "E0277"
            | "E0308"
            | "E0373"
            | "E0382"
            | "E0499"
            | "E0502"
            | "E0503"
            | "E0505"
            | "E0506"
            | "E0507"
            | "E0515"
            | "E0521"
            | "E0594"
            | "E0596"
            | "E0597"
            | "E0599"
            | "E0716"
            | "E0728"
            | "E0733"
    )
}

#[cfg(test)]
mod tests {
    use super::{is_teachable_rust_diagnostic, should_record_invalid_use};
    use crate::diagnostics::OwnershipModelArtifact;

    #[test]
    fn invalid_use_ignores_secondary_e0382_hints() {
        assert!(should_record_invalid_use(Some(lsp_types::DiagnosticSeverity::Error)));
        assert!(!should_record_invalid_use(Some(lsp_types::DiagnosticSeverity::Hint)));
        assert!(!should_record_invalid_use(Some(lsp_types::DiagnosticSeverity::Information)));
    }

    #[test]
    fn learning_problem_codes_cover_the_beginner_error_families() {
        for code in [
            "E0106", "E0277", "E0308", "E0373", "E0382", "E0499", "E0502", "E0503", "E0505",
            "E0506", "E0507", "E0515", "E0521", "E0594", "E0596", "E0597", "E0599", "E0716",
            "E0728", "E0733",
        ] {
            assert!(is_teachable_rust_diagnostic(code), "missing {code}");
        }
        assert!(!is_teachable_rust_diagnostic("unused_variables"));
        assert!(!is_teachable_rust_diagnostic("E9999"));
    }

    #[test]
    fn schema_six_memory_graph_deserializes_and_unknown_state_fails_safely() {
        let artifact = r#"{
            "schema_version": 6,
            "sources": [],
            "ownership_events": [],
            "memory_graph": {
                "nodes": [{
                    "id": "node-a",
                    "body_id": 1,
                    "place": "a",
                    "kind": "binding",
                    "storage": "stack",
                    "label": "stack binding `a`",
                    "type_name": "Box<i32>",
                    "size": 8,
                    "align": 8,
                    "span": null,
                    "state": "available",
                    "provenance": "exact",
                    "physical_placement_note": "source model",
                    "truncated": false
                }],
                "edges": [],
                "snapshots": [],
                "access_paths": [],
                "truncated": false
            }
        }"#;
        let parsed: OwnershipModelArtifact = serde_json::from_str(artifact).unwrap();
        assert_eq!(parsed.schema_version, 6);
        assert_eq!(parsed.memory_graph.nodes[0].id, "node-a");

        let invalid = artifact.replace("\"available\"", "\"future_state\"");
        assert!(serde_json::from_str::<OwnershipModelArtifact>(&invalid).is_err());
    }
}
