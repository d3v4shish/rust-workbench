use ::serde::{Deserialize, Serialize};
use anyhow::Context as _;
use gpui::{App, AppContext as _, AsyncApp, Entity, Task, WeakEntity};
use language::{Buffer, File as _, ServerHealth};
use lsp::{DEFAULT_LSP_REQUEST_TIMEOUT, LanguageServer, LanguageServerId, LanguageServerName};
use rpc::proto;
use text::PointUtf16;

use crate::{CodeAction, LspAction, LspStore, LspStoreEvent, Project, ProjectPath, lsp_store};

pub const RUST_ANALYZER_NAME: LanguageServerName = LanguageServerName::new_static("rust-analyzer");
pub const CARGO_DIAGNOSTICS_SOURCE_NAME: &str = "rustc";

/// Experimental: Informs the end user about the state of the server
///
/// [Rust Analyzer Specification](https://rust-analyzer.github.io/book/contributing/lsp-extensions.html#server-status)
#[derive(Debug)]
enum ServerStatus {}

#[derive(Debug, PartialEq, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ServerStatusParams {
    pub health: ServerHealth,
    pub message: Option<String>,
}

pub enum OwnershipModelRequest {}

impl lsp::request::Request for OwnershipModelRequest {
    type Params = lsp::TextDocumentPositionParams;
    type Result = OwnershipModel;
    const METHOD: &'static str = "rust-analyzer/ownershipModel";
}

pub enum OwnershipProblemsRequest {}

impl lsp::request::Request for OwnershipProblemsRequest {
    type Params = lsp::TextDocumentIdentifier;
    type Result = OwnershipProblems;
    const METHOD: &'static str = "rust-analyzer/ownershipProblems";
}

pub enum OwnershipRepairRequest {}

impl lsp::request::Request for OwnershipRepairRequest {
    type Params = OwnershipRepairParams;
    type Result = Option<lsp::CodeAction>;
    const METHOD: &'static str = "rust-analyzer/ownershipRepair";
}

pub enum OwnershipValidateRepairRequest {}

impl lsp::request::Request for OwnershipValidateRepairRequest {
    type Params = OwnershipRepairParams;
    type Result = OwnershipRepairValidationResult;
    const METHOD: &'static str = "rust-analyzer/ownershipValidateRepair";
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipRepairParams {
    pub text_document: lsp::TextDocumentIdentifier,
    pub position: lsp::Position,
    pub repair_id: String,
    pub source_hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipRepairValidationResult {
    pub status: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipProblems {
    pub schema_version: u32,
    pub status: String,
    pub source_hash: String,
    pub problems: Vec<OwnershipProblem>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipProblem {
    pub id: String,
    pub category: String,
    pub diagnostic_code: Option<String>,
    #[serde(default)]
    pub message: String,
    pub binding_name: String,
    pub primary_range: lsp::Range,
    pub binding_range: lsp::Range,
    pub related_ranges: Vec<lsp::Range>,
    #[serde(default)]
    pub related: Vec<OwnershipProblemRelated>,
    pub model_position: lsp::Position,
    pub precision: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipProblemRelated {
    pub message: String,
    pub range: lsp::Range,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipModel {
    pub schema_version: u32,
    pub precision: String,
    pub status: String,
    #[serde(default)]
    pub truncated: bool,
    pub source_hash: String,
    #[serde(default)]
    pub selected_problem_id: Option<String>,
    pub selected_place: Option<String>,
    pub events: Vec<OwnershipEvent>,
    #[serde(default)]
    pub value_trace: Vec<OwnershipValueTraceStep>,
    pub repairs: Vec<OwnershipRepair>,
    #[serde(default)]
    pub bodies: Vec<OwnershipBody>,
    #[serde(default)]
    pub bindings: Vec<OwnershipBinding>,
    #[serde(default)]
    pub loans: Vec<OwnershipLoan>,
    #[serde(default)]
    pub operations: Vec<OwnershipOperationInsight>,
    #[serde(default)]
    pub mutation_requirement: Option<OwnershipMutationRequirement>,
    #[serde(default)]
    pub conflict_graph: Option<OwnershipConflictGraph>,
    #[serde(default)]
    pub source_context: Option<OwnershipSourceContext>,
    #[serde(default)]
    pub c_sketch: Option<OwnershipCSketch>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipSourceContext {
    pub file: String,
    pub breadcrumbs: Vec<OwnershipContextItem>,
    pub call_paths: Vec<Vec<String>>,
    pub related_types: Vec<String>,
    pub provenance: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipContextItem {
    pub kind: String,
    pub label: String,
    pub range: Option<lsp::Range>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipConflictGraph {
    pub title: String,
    pub summary: String,
    pub requested_access: String,
    pub nodes: Vec<OwnershipConflictNode>,
    pub edges: Vec<OwnershipConflictEdge>,
    pub snapshots: Vec<OwnershipConflictSnapshot>,
    pub provenance: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipConflictNode {
    pub id: String,
    pub label: String,
    pub type_name: Option<String>,
    pub role: String,
    pub memory: String,
    pub range: Option<lsp::Range>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipConflictEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub label: String,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipConflictSnapshot {
    pub phase: String,
    pub title: String,
    pub explanation: String,
    pub range: lsp::Range,
    pub states: Vec<OwnershipConflictNodeState>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipConflictNodeState {
    pub node_id: String,
    pub state: String,
    pub explanation: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipOperationInsight {
    pub id: String,
    pub range: lsp::Range,
    pub name: String,
    pub signature: String,
    pub receiver_type: Option<String>,
    pub required_access: String,
    pub available_access: String,
    pub why_required: String,
    pub documentation: Option<String>,
    pub effects: Vec<String>,
    #[serde(default)]
    pub effect_facts: Vec<OwnershipOperationEffect>,
    pub call_chain: Vec<String>,
    pub alternatives: Vec<OwnershipOperationAlternative>,
    pub provenance: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipMutationRequirement {
    pub target_place: String,
    pub access_source: String,
    pub available_access: String,
    pub required_access: String,
    pub operation_id: String,
    pub operation_name: String,
    pub explanation: String,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipOperationEffect {
    pub kind: String,
    pub summary: String,
    pub certainty: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipOperationAlternative {
    pub name: String,
    pub signature: String,
    pub access: String,
    pub behavior: String,
    pub difference: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipBody {
    pub body_id: u64,
    pub name: String,
    pub range: lsp::Range,
    pub blocks: Vec<OwnershipBlock>,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipBlock {
    pub basic_block: u32,
    pub range: lsp::Range,
    pub successors: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipBinding {
    pub id: String,
    pub body_id: u64,
    pub name: String,
    pub range: lsp::Range,
    pub type_name: String,
    pub size: Option<u64>,
    pub align: Option<u64>,
    pub memory_layers: Vec<OwnershipMemoryLayer>,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipMemoryLayer {
    pub kind: String,
    pub storage: String,
    pub label: String,
    pub type_name: String,
    pub size: Option<u64>,
    pub align: Option<u64>,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipLoanPoint {
    pub basic_block: u32,
    pub statement_index: u32,
    pub range: lsp::Range,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipLoan {
    pub body_id: u64,
    pub loan_id: u32,
    pub kind: String,
    pub name: String,
    pub place: String,
    pub reserve: OwnershipLoanPoint,
    pub activation: Option<OwnershipLoanPoint>,
    pub live_points: Vec<OwnershipLoanPoint>,
    pub end_points: Vec<OwnershipLoanPoint>,
    pub truncated: bool,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipCSketch {
    pub title: String,
    pub code: String,
    pub warning: String,
    pub linked_event_ids: Vec<String>,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipEvent {
    pub event_id: String,
    pub body_id: u64,
    pub basic_block: u32,
    pub statement_index: u32,
    pub kind: String,
    pub state: String,
    pub place: String,
    pub loan_id: Option<u32>,
    pub range: lsp::Range,
    pub binding_range: lsp::Range,
    pub detail: Option<String>,
    #[serde(default)]
    pub destination: Option<OwnershipEventDestination>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipEventDestination {
    pub kind: String,
    pub label: String,
    pub place: Option<String>,
    pub range: Option<lsp::Range>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipValueTraceStep {
    pub id: String,
    pub kind: String,
    pub range: lsp::Range,
    pub from_label: String,
    pub to_label: Option<String>,
    pub source_state: String,
    pub destination_state: Option<String>,
    pub allocation_effect: String,
    pub explanation: String,
    pub provenance: String,
    pub control_flow: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipRepair {
    pub id: String,
    pub title: String,
    pub strategy: String,
    pub semantics: String,
    pub diff: String,
    pub compiler_validated: bool,
    #[serde(default = "candidate_validation_state")]
    pub validation_state: String,
    #[serde(default)]
    pub effects: OwnershipRepairEffects,
}

fn candidate_validation_state() -> String {
    "candidate".to_owned()
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipRepairEffects {
    pub ownership: String,
    pub mutation: String,
    pub thread_safety: String,
    pub runtime_risk: String,
    pub cost: String,
}

enum OwnershipModelChangedNotification {}

impl lsp::notification::Notification for OwnershipModelChangedNotification {
    type Params = OwnershipModelChangedParams;
    const METHOD: &'static str = "rust-analyzer/ownershipModelChanged";
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct OwnershipModelChangedParams {
    uri: lsp::Uri,
    schema_version: u32,
    status: String,
    source_hash: String,
}

impl lsp::notification::Notification for ServerStatus {
    type Params = ServerStatusParams;
    const METHOD: &'static str = "experimental/serverStatus";
}

pub fn register_notifications(lsp_store: WeakEntity<LspStore>, language_server: &LanguageServer) {
    let name = language_server.name();
    let server_id = language_server.server_id();
    let status_lsp_store = lsp_store.clone();

    language_server
        .on_notification::<ServerStatus, _>({
            move |params, cx| {
                let message = params.message;
                let log_message = message.as_ref().map(|message| {
                    format!("Language server {name} (id {server_id}) status update: {message}")
                });
                let status = match &params.health {
                    ServerHealth::Ok => {
                        if let Some(log_message) = log_message {
                            log::info!("{log_message}");
                        }
                        proto::ServerHealth::Ok
                    }
                    ServerHealth::Warning => {
                        if let Some(log_message) = log_message {
                            log::warn!("{log_message}");
                        }
                        proto::ServerHealth::Warning
                    }
                    ServerHealth::Error => {
                        if let Some(log_message) = log_message {
                            log::error!("{log_message}");
                        }
                        proto::ServerHealth::Error
                    }
                };

                status_lsp_store
                    .update(cx, |_, cx| {
                        cx.emit(LspStoreEvent::LanguageServerUpdate {
                            language_server_id: server_id,
                            name: Some(name.clone()),
                            message: proto::update_language_server::Variant::StatusUpdate(
                                proto::StatusUpdate {
                                    message,
                                    status: Some(proto::status_update::Status::Health(
                                        status as i32,
                                    )),
                                },
                            ),
                        });
                    })
                    .ok();
            }
        })
        .detach();

    language_server
        .on_notification::<OwnershipModelChangedNotification, _>({
            move |params, cx| {
                lsp_store
                    .update(cx, |_, cx| {
                        cx.emit(LspStoreEvent::OwnershipModelChanged {
                            uri: params.uri,
                            schema_version: params.schema_version,
                            status: params.status,
                            source_hash: params.source_hash,
                        });
                    })
                    .ok();
            }
        })
        .detach();
}

pub fn ownership_model(
    project: Entity<Project>,
    buffer: Entity<Buffer>,
    position: PointUtf16,
    cx: &mut App,
) -> Task<anyhow::Result<OwnershipModel>> {
    let request = project.read_with(cx, |project, cx| {
        let server_id =
            project.language_server_id_for_name(buffer.read(cx), &RUST_ANALYZER_NAME, cx)?;
        let language_server = project
            .lsp_store()
            .read(cx)
            .language_server_for_id(server_id)?;
        let file = worktree::File::from_dyn(buffer.read(cx).file())?.as_local()?;
        let uri = lsp::Uri::from_file_path(file.abs_path(cx)).ok()?;
        Some(language_server.request::<OwnershipModelRequest>(
            lsp::TextDocumentPositionParams {
                text_document: lsp::TextDocumentIdentifier { uri },
                position: language::point_to_lsp(position),
            },
            DEFAULT_LSP_REQUEST_TIMEOUT,
        ))
    });
    let Some(request) = request else {
        return Task::ready(Ok(OwnershipModel {
            status: "rust_analyzer_unavailable".to_owned(),
            ..OwnershipModel::default()
        }));
    };
    cx.background_spawn(async move {
        request
            .await
            .into_response()
            .context("requesting compiler ownership model from rust-analyzer")
    })
}

pub fn ownership_problems(
    project: Entity<Project>,
    buffer: Entity<Buffer>,
    cx: &mut App,
) -> Task<anyhow::Result<OwnershipProblems>> {
    let request = project.read_with(cx, |project, cx| {
        let server_id =
            project.language_server_id_for_name(buffer.read(cx), &RUST_ANALYZER_NAME, cx)?;
        let language_server = project
            .lsp_store()
            .read(cx)
            .language_server_for_id(server_id)?;
        let file = worktree::File::from_dyn(buffer.read(cx).file())?.as_local()?;
        let uri = lsp::Uri::from_file_path(file.abs_path(cx)).ok()?;
        Some(language_server.request::<OwnershipProblemsRequest>(
            lsp::TextDocumentIdentifier { uri },
            DEFAULT_LSP_REQUEST_TIMEOUT,
        ))
    });
    let Some(request) = request else {
        return Task::ready(Ok(OwnershipProblems {
            status: "rust_analyzer_unavailable".to_owned(),
            ..OwnershipProblems::default()
        }));
    };
    cx.background_spawn(async move {
        request
            .await
            .into_response()
            .context("requesting ownership problems from rust-analyzer")
    })
}

pub fn ownership_repair(
    project: Entity<Project>,
    buffer: Entity<Buffer>,
    position: PointUtf16,
    repair_id: String,
    source_hash: String,
    cx: &mut App,
) -> Task<anyhow::Result<Option<CodeAction>>> {
    let request = project.read_with(cx, |project, cx| {
        let server_id =
            project.language_server_id_for_name(buffer.read(cx), &RUST_ANALYZER_NAME, cx)?;
        let language_server = project
            .lsp_store()
            .read(cx)
            .language_server_for_id(server_id)?;
        let file = worktree::File::from_dyn(buffer.read(cx).file())?.as_local()?;
        let uri = lsp::Uri::from_file_path(file.abs_path(cx)).ok()?;
        let anchor = buffer.read(cx).anchor_before(position);
        let request = language_server.request::<OwnershipRepairRequest>(
            OwnershipRepairParams {
                text_document: lsp::TextDocumentIdentifier { uri },
                position: language::point_to_lsp(position),
                repair_id,
                source_hash,
            },
            DEFAULT_LSP_REQUEST_TIMEOUT,
        );
        Some((server_id, anchor, request))
    });
    let Some((server_id, anchor, request)) = request else {
        return Task::ready(Ok(None));
    };
    cx.background_spawn(async move {
        let action = request
            .await
            .into_response()
            .context("requesting a source-validated ownership repair from rust-analyzer")?;
        Ok(action.map(|action| CodeAction {
            server_id,
            range: anchor..anchor,
            lsp_action: LspAction::Action(Box::new(action)),
            resolved: true,
        }))
    })
}

pub fn ownership_validate_repair(
    project: Entity<Project>,
    buffer: Entity<Buffer>,
    position: PointUtf16,
    repair_id: String,
    source_hash: String,
    cx: &mut App,
) -> Task<anyhow::Result<OwnershipRepairValidationResult>> {
    let request = project.read_with(cx, |project, cx| {
        let server_id =
            project.language_server_id_for_name(buffer.read(cx), &RUST_ANALYZER_NAME, cx)?;
        let language_server = project
            .lsp_store()
            .read(cx)
            .language_server_for_id(server_id)?;
        let file = worktree::File::from_dyn(buffer.read(cx).file())?.as_local()?;
        let uri = lsp::Uri::from_file_path(file.abs_path(cx)).ok()?;
        Some(language_server.request::<OwnershipValidateRepairRequest>(
            OwnershipRepairParams {
                text_document: lsp::TextDocumentIdentifier { uri },
                position: language::point_to_lsp(position),
                repair_id,
                source_hash,
            },
            DEFAULT_LSP_REQUEST_TIMEOUT,
        ))
    });
    let Some(request) = request else {
        return Task::ready(Ok(OwnershipRepairValidationResult {
            status: "unavailable".to_owned(),
            message: "rust-analyzer is not available for this buffer.".to_owned(),
        }));
    };
    cx.background_spawn(async move {
        request
            .await
            .into_response()
            .context("starting compiler validation for an ownership repair")
    })
}

pub fn cancel_flycheck(
    project: Entity<Project>,
    buffer_path: Option<ProjectPath>,
    cx: &mut App,
) -> Task<anyhow::Result<()>> {
    let upstream_client = project.read(cx).lsp_store().read(cx).upstream_client();
    let lsp_store = project.read(cx).lsp_store();
    let buffer = buffer_path.map(|buffer_path| {
        project.update(cx, |project, cx| {
            project.buffer_store().update(cx, |buffer_store, cx| {
                buffer_store.open_buffer(buffer_path, cx)
            })
        })
    });

    cx.spawn(async move |cx| {
        let buffer = match buffer {
            Some(buffer) => Some(buffer.await?),
            None => None,
        };
        let Some(rust_analyzer_server) = find_rust_analyzer_server(&project, buffer.as_ref(), cx)
        else {
            return Ok(());
        };

        if let Some((client, project_id)) = upstream_client {
            let request = proto::LspExtCancelFlycheck {
                project_id,
                language_server_id: rust_analyzer_server.to_proto(),
            };
            client
                .request(request)
                .await
                .context("lsp ext cancel flycheck proto request")?;
        } else {
            lsp_store
                .read_with(cx, |lsp_store, _| {
                    if let Some(server) = lsp_store.language_server_for_id(rust_analyzer_server) {
                        server.notify::<lsp_store::lsp_ext_command::LspExtCancelFlycheck>(())
                    } else {
                        Ok(())
                    }
                })
                .context("lsp ext cancel flycheck")?;
        };
        anyhow::Ok(())
    })
}

pub fn run_flycheck(
    project: Entity<Project>,
    buffer_path: Option<ProjectPath>,
    cx: &mut App,
) -> Task<anyhow::Result<()>> {
    let upstream_client = project.read(cx).lsp_store().read(cx).upstream_client();
    let lsp_store = project.read(cx).lsp_store();
    let buffer = buffer_path.map(|buffer_path| {
        project.update(cx, |project, cx| {
            project.buffer_store().update(cx, |buffer_store, cx| {
                buffer_store.open_buffer(buffer_path, cx)
            })
        })
    });

    cx.spawn(async move |cx| {
        let buffer = match buffer {
            Some(buffer) => Some(buffer.await?),
            None => None,
        };
        let Some(rust_analyzer_server) = find_rust_analyzer_server(&project, buffer.as_ref(), cx)
        else {
            return Ok(());
        };

        if let Some((client, project_id)) = upstream_client {
            let buffer_id = buffer
                .map(|buffer| buffer.read_with(cx, |buffer, _| buffer.remote_id().to_proto()));
            let request = proto::LspExtRunFlycheck {
                project_id,
                buffer_id,
                language_server_id: rust_analyzer_server.to_proto(),
                current_file_only: false,
            };
            client
                .request(request)
                .await
                .context("lsp ext run flycheck proto request")?;
        } else {
            lsp_store
                .read_with(cx, |lsp_store, _| {
                    if let Some(server) = lsp_store.language_server_for_id(rust_analyzer_server) {
                        server.notify::<lsp_store::lsp_ext_command::LspExtRunFlycheck>(
                            lsp_store::lsp_ext_command::RunFlycheckParams {
                                text_document: None,
                            },
                        )
                    } else {
                        Ok(())
                    }
                })
                .context("lsp ext run flycheck")?;
        };
        anyhow::Ok(())
    })
}

pub fn clear_flycheck(
    project: Entity<Project>,
    buffer_path: Option<ProjectPath>,
    cx: &mut App,
) -> Task<anyhow::Result<()>> {
    let upstream_client = project.read(cx).lsp_store().read(cx).upstream_client();
    let lsp_store = project.read(cx).lsp_store();
    let buffer = buffer_path.map(|buffer_path| {
        project.update(cx, |project, cx| {
            project.buffer_store().update(cx, |buffer_store, cx| {
                buffer_store.open_buffer(buffer_path, cx)
            })
        })
    });

    cx.spawn(async move |cx| {
        let buffer = match buffer {
            Some(buffer) => Some(buffer.await?),
            None => None,
        };
        let Some(rust_analyzer_server) = find_rust_analyzer_server(&project, buffer.as_ref(), cx)
        else {
            return Ok(());
        };

        if let Some((client, project_id)) = upstream_client {
            let request = proto::LspExtClearFlycheck {
                project_id,
                language_server_id: rust_analyzer_server.to_proto(),
            };
            client
                .request(request)
                .await
                .context("lsp ext clear flycheck proto request")?;
        } else {
            lsp_store
                .read_with(cx, |lsp_store, _| {
                    if let Some(server) = lsp_store.language_server_for_id(rust_analyzer_server) {
                        server.notify::<lsp_store::lsp_ext_command::LspExtClearFlycheck>(())
                    } else {
                        Ok(())
                    }
                })
                .context("lsp ext clear flycheck")?;
        };
        anyhow::Ok(())
    })
}

fn find_rust_analyzer_server(
    project: &Entity<Project>,
    buffer: Option<&Entity<Buffer>>,
    cx: &mut AsyncApp,
) -> Option<LanguageServerId> {
    project.read_with(cx, |project, cx| {
        buffer
            .and_then(|buffer| {
                project.language_server_id_for_name(buffer.read(cx), &RUST_ANALYZER_NAME, cx)
            })
            // If no rust-analyzer found for the current buffer (e.g. `settings.json`), fall back to the project lookup
            // and use project's rust-analyzer if it's the only one.
            .or_else(|| {
                let rust_analyzer_servers = project
                    .lsp_store()
                    .read(cx)
                    .language_server_statuses
                    .iter()
                    .filter_map(|(server_id, server_status)| {
                        if server_status.name == RUST_ANALYZER_NAME {
                            Some(*server_id)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                if rust_analyzer_servers.len() == 1 {
                    rust_analyzer_servers.first().copied()
                } else {
                    None
                }
            })
    })
}
