use ::serde::{Deserialize, Serialize};
use anyhow::{Context as _, ensure};
use gpui::{App, AppContext as _, AsyncApp, Entity, Task, WeakEntity};
use language::{Buffer, File as _, ServerHealth};
use lsp::{DEFAULT_LSP_REQUEST_TIMEOUT, LanguageServer, LanguageServerId, LanguageServerName};
use rpc::proto;
use text::PointUtf16;

use crate::{CodeAction, LspAction, LspStore, LspStoreEvent, Project, ProjectPath, lsp_store};

pub const RUST_ANALYZER_NAME: LanguageServerName = LanguageServerName::new_static("rust-analyzer");
pub const CARGO_DIAGNOSTICS_SOURCE_NAME: &str = "rustc";

const MAX_OWNERSHIP_PROBLEMS: usize = 512;
const MAX_OWNERSHIP_EVENTS: usize = 256;
const MAX_OWNERSHIP_BODIES: usize = 512;
const MAX_OWNERSHIP_BINDINGS: usize = 64;
const MAX_OWNERSHIP_LOANS: usize = 64;
const MAX_OWNERSHIP_OPERATIONS: usize = 64;
const MAX_OWNERSHIP_REPAIRS: usize = 16;
const MAX_OWNERSHIP_GRAPH_NODES: usize = 128;
const MAX_OWNERSHIP_GRAPH_EDGES: usize = 256;
const MAX_OWNERSHIP_SNAPSHOTS: usize = 256;
const MAX_OWNERSHIP_ACCESS_PATHS: usize = 64;
const MAX_OWNERSHIP_WORKSPACE_CLUSTERS: usize = 64;
const MAX_OWNERSHIP_TEXT_BYTES: usize = 256 * 1024;

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

pub enum OwnershipWorkspaceGuideRequest {}

impl lsp::request::Request for OwnershipWorkspaceGuideRequest {
    type Params = OwnershipWorkspaceGuideParams;
    type Result = OwnershipWorkspaceGuide;
    const METHOD: &'static str = "rust-analyzer/ownershipWorkspaceGuide";
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
pub struct OwnershipWorkspaceGuideParams {
    pub text_document: lsp::TextDocumentIdentifier,
    pub position: lsp::Position,
    pub selected_problem_id: Option<String>,
    pub expected_revision: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipWorkspaceGuide {
    pub schema_version: u32,
    pub status: String,
    pub revision: String,
    pub selected_cluster_id: Option<String>,
    pub clusters: Vec<OwnershipProblemCluster>,
    pub journey: Vec<OwnershipJourneyFrame>,
    pub intent_question: Option<OwnershipIntentQuestion>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipProblemCluster {
    pub id: String,
    pub root_problem_id: String,
    pub title: String,
    pub summary: String,
    pub category: String,
    pub diagnostic_code: Option<String>,
    pub root: OwnershipWorkspaceSite,
    pub impacts: Vec<OwnershipWorkspaceSite>,
    pub related_constraints: Vec<OwnershipWorkspaceSite>,
    pub affected_files: usize,
    pub precision: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipWorkspaceSite {
    pub problem_id: Option<String>,
    pub role: String,
    pub location: lsp::Location,
    pub label: String,
    pub relationship: String,
    pub precision: String,
    pub selected: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipJourneyFrame {
    pub id: String,
    pub kind: String,
    pub location: lsp::Location,
    pub label: String,
    pub explanation: String,
    pub transfer: String,
    pub after: String,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipIntentQuestion {
    pub id: String,
    pub prompt: String,
    pub choices: Vec<OwnershipIntentChoice>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipIntentChoice {
    pub id: String,
    pub label: String,
    pub consequence: String,
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
    #[serde(default)]
    pub compiler_schema_version: u32,
    #[serde(default)]
    pub target_triple: String,
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
    pub memory_graph: OwnershipMemoryGraph,
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
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub ownership_relevant: bool,
    #[serde(default)]
    pub receiver_flow: Option<OwnershipOperationReceiver>,
    #[serde(default)]
    pub argument_flows: Vec<OwnershipOperationArgument>,
    #[serde(default)]
    pub return_flow: Option<OwnershipOperationReturn>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipOperationReceiver {
    pub expression: String,
    pub range: lsp::Range,
    pub transfer: String,
    pub after: String,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipOperationArgument {
    pub index: usize,
    pub expression: String,
    pub range: lsp::Range,
    pub parameter_type: String,
    pub transfer: String,
    pub after: String,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipOperationReturn {
    pub type_name: String,
    pub kind: String,
    pub borrowed_from: Option<String>,
    pub after: String,
    pub provenance: String,
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipMemoryGraph {
    pub nodes: Vec<OwnershipMemoryNode>,
    pub edges: Vec<OwnershipMemoryEdge>,
    pub snapshots: Vec<OwnershipMemorySnapshot>,
    pub access_paths: Vec<OwnershipAccessPath>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipMemoryNode {
    pub id: String,
    pub body_id: u64,
    pub place: String,
    pub kind: String,
    pub storage: String,
    pub label: String,
    pub type_name: String,
    pub size: Option<u64>,
    pub align: Option<u64>,
    pub range: Option<lsp::Range>,
    pub state: String,
    pub provenance: String,
    pub physical_placement_note: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipMemoryEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub relation: String,
    pub event_id: Option<String>,
    pub loan_id: Option<u32>,
    pub range: Option<lsp::Range>,
    pub provenance: String,
    pub path_marker: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipStateDelta {
    pub node_id: String,
    pub from: Option<String>,
    pub to: String,
    pub relation_added: Option<String>,
    pub relation_removed: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipMemorySnapshot {
    pub id: String,
    pub event_id: String,
    pub body_id: u64,
    pub basic_block: u32,
    pub statement_index: u32,
    pub kind: String,
    pub range: lsp::Range,
    pub place: String,
    pub loan_id: Option<u32>,
    pub path_marker: Option<String>,
    pub deltas: Vec<OwnershipStateDelta>,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipAccessStep {
    pub kind: String,
    pub starting_type: String,
    pub result_type: String,
    pub mutability: String,
    pub explicitness: String,
    pub fallible: bool,
    pub may_panic: bool,
    pub requires_unsafe: bool,
    pub explanation: String,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipAccessPath {
    pub id: String,
    pub body_id: u64,
    pub node_id: String,
    pub place: String,
    pub purpose: String,
    pub steps: Vec<OwnershipAccessStep>,
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
    #[serde(default)]
    pub affected_files: Vec<String>,
    #[serde(default)]
    pub preview_complete: bool,
    pub compiler_validated: bool,
    #[serde(default = "candidate_validation_state")]
    pub validation_state: String,
    #[serde(default)]
    pub effects: OwnershipRepairEffects,
    #[serde(default)]
    pub preview_graph: Option<OwnershipMemoryGraph>,
}

fn candidate_validation_state() -> String {
    "candidate".to_owned()
}

fn validate_range(label: &str, range: &lsp::Range) -> anyhow::Result<()> {
    ensure!(
        (range.start.line, range.start.character) <= (range.end.line, range.end.character),
        "{label} contains an inverted LSP range"
    );
    Ok(())
}

fn validate_text(label: &str, text: &str) -> anyhow::Result<()> {
    ensure!(
        text.len() <= MAX_OWNERSHIP_TEXT_BYTES,
        "{label} exceeds the Rust Workbench text limit"
    );
    Ok(())
}

fn validate_schema(
    label: &str,
    schema_version: u32,
    maximum: u32,
    unavailable: bool,
) -> anyhow::Result<()> {
    if unavailable && schema_version == 0 {
        return Ok(());
    }
    ensure!(schema_version > 0, "{label} omitted its schema version");
    ensure!(
        schema_version <= maximum,
        "{label} schema {schema_version} is newer than supported schema {maximum}"
    );
    Ok(())
}

fn validate_unique_ids<'a>(
    label: &str,
    ids: impl IntoIterator<Item = &'a str>,
) -> anyhow::Result<()> {
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        ensure!(!id.is_empty(), "{label} contains an empty id");
        ensure!(seen.insert(id), "{label} contains duplicate id `{id}`");
    }
    Ok(())
}

fn validate_memory_graph(label: &str, graph: &OwnershipMemoryGraph) -> anyhow::Result<()> {
    ensure!(
        graph.nodes.len() <= MAX_OWNERSHIP_GRAPH_NODES,
        "{label} contains too many memory nodes"
    );
    ensure!(
        graph.edges.len() <= MAX_OWNERSHIP_GRAPH_EDGES,
        "{label} contains too many memory edges"
    );
    ensure!(
        graph.snapshots.len() <= MAX_OWNERSHIP_SNAPSHOTS,
        "{label} contains too many memory snapshots"
    );
    ensure!(
        graph.access_paths.len() <= MAX_OWNERSHIP_ACCESS_PATHS,
        "{label} contains too many access paths"
    );
    validate_unique_ids(
        &format!("{label} nodes"),
        graph.nodes.iter().map(|node| node.id.as_str()),
    )?;
    validate_unique_ids(
        &format!("{label} edges"),
        graph.edges.iter().map(|edge| edge.id.as_str()),
    )?;
    validate_unique_ids(
        &format!("{label} snapshots"),
        graph.snapshots.iter().map(|snapshot| snapshot.id.as_str()),
    )?;
    validate_unique_ids(
        &format!("{label} access paths"),
        graph.access_paths.iter().map(|path| path.id.as_str()),
    )?;
    for node in &graph.nodes {
        if let Some(range) = &node.range {
            validate_range("ownership memory node", range)?;
        }
    }
    for edge in &graph.edges {
        if let Some(range) = &edge.range {
            validate_range("ownership memory edge", range)?;
        }
    }
    for snapshot in &graph.snapshots {
        validate_range("ownership memory snapshot", &snapshot.range)?;
        ensure!(
            snapshot.deltas.len() <= MAX_OWNERSHIP_GRAPH_NODES,
            "{label} snapshot contains too many state changes"
        );
    }
    for path in &graph.access_paths {
        ensure!(
            path.steps.len() <= 32,
            "{label} access path contains too many steps"
        );
    }
    Ok(())
}

fn validate_ownership_problems(result: &OwnershipProblems) -> anyhow::Result<()> {
    let unavailable = result.status == "rust_analyzer_unavailable";
    ensure!(
        matches!(
            result.status.as_str(),
            "ready" | "waiting_for_compiler" | "rust_analyzer_unavailable"
        ),
        "ownership problems returned unknown status `{}`",
        result.status
    );
    validate_schema("ownership problems", result.schema_version, 7, unavailable)?;
    ensure!(
        result.problems.len() <= MAX_OWNERSHIP_PROBLEMS,
        "ownership problems response exceeds {MAX_OWNERSHIP_PROBLEMS} items"
    );
    validate_unique_ids(
        "ownership problems",
        result.problems.iter().map(|problem| problem.id.as_str()),
    )?;
    for problem in &result.problems {
        validate_range("ownership problem", &problem.primary_range)?;
        validate_range("ownership problem binding", &problem.binding_range)?;
        ensure!(
            problem.related_ranges.len() <= 64,
            "ownership problem has too many related ranges"
        );
        ensure!(
            problem.related.len() <= 64,
            "ownership problem has too many related messages"
        );
        for range in &problem.related_ranges {
            validate_range("ownership problem related site", range)?;
        }
        for related in &problem.related {
            validate_range("ownership problem related message", &related.range)?;
            validate_text("ownership problem related message", &related.message)?;
        }
        validate_text("ownership problem message", &problem.message)?;
    }
    Ok(())
}

fn validate_workspace_guide(result: &OwnershipWorkspaceGuide) -> anyhow::Result<()> {
    let unavailable = result.status == "rust_analyzer_unavailable";
    ensure!(
        matches!(
            result.status.as_str(),
            "ready" | "ready_empty" | "refreshed" | "rust_analyzer_unavailable"
        ),
        "workspace guide returned unknown status `{}`",
        result.status
    );
    validate_schema("workspace guide", result.schema_version, 1, unavailable)?;
    ensure!(
        result.clusters.len() <= MAX_OWNERSHIP_WORKSPACE_CLUSTERS,
        "workspace guide contains too many clusters"
    );
    ensure!(
        result.journey.len() <= 64,
        "workspace guide contains too many journey frames"
    );
    validate_unique_ids(
        "workspace guide clusters",
        result.clusters.iter().map(|cluster| cluster.id.as_str()),
    )?;
    validate_unique_ids(
        "workspace guide journey",
        result.journey.iter().map(|frame| frame.id.as_str()),
    )?;
    for cluster in &result.clusters {
        ensure!(
            cluster.impacts.len() <= 64,
            "workspace cluster contains too many impacts"
        );
        ensure!(
            cluster.related_constraints.len() <= 64,
            "workspace cluster contains too many related constraints"
        );
        validate_range("workspace cluster root", &cluster.root.location.range)?;
        for site in cluster.impacts.iter().chain(&cluster.related_constraints) {
            validate_range("workspace guide site", &site.location.range)?;
        }
    }
    for frame in &result.journey {
        validate_range("workspace guide journey frame", &frame.location.range)?;
    }
    if let Some(question) = &result.intent_question {
        ensure!(
            question.choices.len() <= 16,
            "workspace guide contains too many intent choices"
        );
    }
    Ok(())
}

fn validate_ownership_model(result: &OwnershipModel) -> anyhow::Result<()> {
    let unavailable = result.status == "rust_analyzer_unavailable";
    ensure!(
        matches!(
            result.status.as_str(),
            "ready" | "waiting_for_compiler" | "rust_analyzer_unavailable"
        ),
        "ownership model returned unknown status `{}`",
        result.status
    );
    validate_schema("ownership model", result.schema_version, 14, unavailable)?;
    ensure!(
        result.compiler_schema_version <= 7,
        "compiler ownership schema {} is newer than supported schema 7",
        result.compiler_schema_version
    );
    ensure!(
        result.events.len() <= MAX_OWNERSHIP_EVENTS,
        "ownership model contains too many events"
    );
    ensure!(
        result.value_trace.len() <= 64,
        "ownership model contains too many value trace steps"
    );
    ensure!(
        result.bodies.len() <= MAX_OWNERSHIP_BODIES,
        "ownership model contains too many bodies"
    );
    ensure!(
        result.bindings.len() <= MAX_OWNERSHIP_BINDINGS,
        "ownership model contains too many bindings"
    );
    ensure!(
        result.loans.len() <= MAX_OWNERSHIP_LOANS,
        "ownership model contains too many loans"
    );
    ensure!(
        result.operations.len() <= MAX_OWNERSHIP_OPERATIONS,
        "ownership model contains too many operations"
    );
    ensure!(
        result.repairs.len() <= MAX_OWNERSHIP_REPAIRS,
        "ownership model contains too many repairs"
    );
    validate_unique_ids(
        "ownership events",
        result.events.iter().map(|event| event.event_id.as_str()),
    )?;
    validate_unique_ids(
        "ownership repairs",
        result.repairs.iter().map(|repair| repair.id.as_str()),
    )?;
    for event in &result.events {
        validate_range("ownership event", &event.range)?;
        validate_range("ownership event binding", &event.binding_range)?;
    }
    for step in &result.value_trace {
        validate_range("ownership value trace", &step.range)?;
    }
    for body in &result.bodies {
        validate_range("ownership body", &body.range)?;
        ensure!(
            body.blocks.len() <= 512,
            "ownership body contains too many blocks"
        );
        for block in &body.blocks {
            validate_range("ownership block", &block.range)?;
            ensure!(
                block.successors.len() <= 32,
                "ownership block contains too many successors"
            );
        }
    }
    for binding in &result.bindings {
        validate_range("ownership binding", &binding.range)?;
        ensure!(
            binding.memory_layers.len() <= 32,
            "ownership binding contains too many storage layers"
        );
    }
    for loan in &result.loans {
        validate_range("ownership loan reservation", &loan.reserve.range)?;
        if let Some(activation) = &loan.activation {
            validate_range("ownership loan activation", &activation.range)?;
        }
        ensure!(
            loan.live_points.len() <= 512,
            "ownership loan contains too many live points"
        );
        ensure!(
            loan.end_points.len() <= 512,
            "ownership loan contains too many end points"
        );
    }
    for operation in &result.operations {
        validate_range("ownership operation", &operation.range)?;
        ensure!(
            operation.argument_flows.len() <= 32,
            "ownership operation contains too many arguments"
        );
        ensure!(
            operation.effects.len() <= 64,
            "ownership operation contains too many effects"
        );
        ensure!(
            operation.effect_facts.len() <= 64,
            "ownership operation contains too many effect facts"
        );
        ensure!(
            operation.alternatives.len() <= 32,
            "ownership operation contains too many alternatives"
        );
    }
    for repair in &result.repairs {
        validate_text("ownership repair preview", &repair.diff)?;
        ensure!(
            repair.affected_files.len() <= 64,
            "ownership repair touches too many files"
        );
        if let Some(graph) = &repair.preview_graph {
            validate_memory_graph("ownership repair preview graph", graph)?;
        }
    }
    validate_memory_graph("ownership memory graph", &result.memory_graph)?;
    if let Some(graph) = &result.conflict_graph {
        ensure!(
            graph.nodes.len() <= 64,
            "ownership conflict graph contains too many nodes"
        );
        ensure!(
            graph.edges.len() <= 96,
            "ownership conflict graph contains too many edges"
        );
        ensure!(
            graph.snapshots.len() <= 64,
            "ownership conflict graph contains too many snapshots"
        );
        validate_unique_ids(
            "ownership conflict graph nodes",
            graph.nodes.iter().map(|node| node.id.as_str()),
        )?;
        for node in &graph.nodes {
            if let Some(range) = &node.range {
                validate_range("ownership conflict node", range)?;
            }
        }
        for snapshot in &graph.snapshots {
            validate_range("ownership conflict snapshot", &snapshot.range)?;
            ensure!(
                snapshot.states.len() <= 64,
                "ownership conflict snapshot contains too many states"
            );
        }
    }
    if let Some(context) = &result.source_context {
        ensure!(
            context.breadcrumbs.len() <= 64,
            "ownership source context has too many breadcrumbs"
        );
        ensure!(
            context.call_paths.len() <= 64,
            "ownership source context has too many call paths"
        );
        ensure!(
            context.related_types.len() <= 64,
            "ownership source context has too many related types"
        );
        for path in &context.call_paths {
            ensure!(
                path.len() <= 32,
                "ownership source context call path is too deep"
            );
        }
    }
    if let Some(sketch) = &result.c_sketch {
        validate_text("ownership conceptual code sketch", &sketch.code)?;
    }
    Ok(())
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
        let result = request
            .await
            .into_response()
            .context("requesting compiler ownership model from rust-analyzer")?;
        validate_ownership_model(&result)?;
        Ok(result)
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
        let result = request
            .await
            .into_response()
            .context("requesting ownership problems from rust-analyzer")?;
        validate_ownership_problems(&result)?;
        Ok(result)
    })
}

pub fn ownership_workspace_guide(
    project: Entity<Project>,
    buffer: Entity<Buffer>,
    position: PointUtf16,
    selected_problem_id: Option<String>,
    expected_revision: Option<String>,
    cx: &mut App,
) -> Task<anyhow::Result<OwnershipWorkspaceGuide>> {
    let request = project.read_with(cx, |project, cx| {
        let server_id =
            project.language_server_id_for_name(buffer.read(cx), &RUST_ANALYZER_NAME, cx)?;
        let language_server = project
            .lsp_store()
            .read(cx)
            .language_server_for_id(server_id)?;
        let file = worktree::File::from_dyn(buffer.read(cx).file())?.as_local()?;
        let uri = lsp::Uri::from_file_path(file.abs_path(cx)).ok()?;
        Some(language_server.request::<OwnershipWorkspaceGuideRequest>(
            OwnershipWorkspaceGuideParams {
                text_document: lsp::TextDocumentIdentifier { uri },
                position: language::point_to_lsp(position),
                selected_problem_id,
                expected_revision,
            },
            DEFAULT_LSP_REQUEST_TIMEOUT,
        ))
    });
    let Some(request) = request else {
        return Task::ready(Ok(OwnershipWorkspaceGuide {
            status: "rust_analyzer_unavailable".to_owned(),
            ..OwnershipWorkspaceGuide::default()
        }));
    };
    cx.background_spawn(async move {
        let result = request
            .await
            .into_response()
            .context("requesting the workspace ownership guide from rust-analyzer")?;
        validate_workspace_guide(&result)?;
        Ok(result)
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
        let result = request
            .await
            .into_response()
            .context("starting compiler validation for an ownership repair")?;
        ensure!(
            matches!(result.status.as_str(), "checking" | "stale" | "unavailable"),
            "ownership repair validation returned unknown status `{}`",
            result.status
        );
        validate_text("ownership repair validation message", &result.message)?;
        Ok(result)
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

#[cfg(test)]
mod ownership_protocol_tests {
    use super::*;

    fn problem(id: String, range: lsp::Range) -> OwnershipProblem {
        OwnershipProblem {
            id,
            category: "use_after_move".to_owned(),
            diagnostic_code: Some("E0382".to_owned()),
            message: "value used after move".to_owned(),
            binding_name: "value".to_owned(),
            primary_range: range,
            binding_range: range,
            related_ranges: Vec::new(),
            related: Vec::new(),
            model_position: range.start,
            precision: "compiler_exact".to_owned(),
        }
    }

    #[test]
    fn unavailable_payloads_may_use_schema_zero() {
        let problems = OwnershipProblems {
            status: "rust_analyzer_unavailable".to_owned(),
            ..Default::default()
        };
        let model = OwnershipModel {
            status: "rust_analyzer_unavailable".to_owned(),
            ..Default::default()
        };
        assert!(validate_ownership_problems(&problems).is_ok());
        assert!(validate_ownership_model(&model).is_ok());
    }

    #[test]
    fn protocol_rejects_newer_schemas_and_inverted_ranges() {
        let model = OwnershipModel {
            schema_version: 15,
            status: "ready".to_owned(),
            ..Default::default()
        };
        assert!(validate_ownership_model(&model).is_err());

        let range = lsp::Range::new(lsp::Position::new(4, 8), lsp::Position::new(3, 1));
        let problems = OwnershipProblems {
            schema_version: 1,
            status: "ready".to_owned(),
            source_hash: "hash".to_owned(),
            problems: vec![problem("problem".to_owned(), range)],
        };
        assert!(validate_ownership_problems(&problems).is_err());
    }

    #[test]
    fn protocol_rejects_duplicate_ids_and_excessive_problem_counts() {
        let range = lsp::Range::new(lsp::Position::new(1, 0), lsp::Position::new(1, 4));
        let duplicate = OwnershipProblems {
            schema_version: 1,
            status: "ready".to_owned(),
            source_hash: "hash".to_owned(),
            problems: vec![
                problem("same".to_owned(), range),
                problem("same".to_owned(), range),
            ],
        };
        assert!(validate_ownership_problems(&duplicate).is_err());

        let excessive = OwnershipProblems {
            schema_version: 1,
            status: "ready".to_owned(),
            source_hash: "hash".to_owned(),
            problems: (0..=MAX_OWNERSHIP_PROBLEMS)
                .map(|index| problem(format!("problem-{index}"), range))
                .collect(),
        };
        assert!(validate_ownership_problems(&excessive).is_err());
    }

    #[test]
    fn protocol_rejects_unknown_statuses_and_oversized_repair_text() {
        let problems = OwnershipProblems {
            schema_version: 1,
            status: "mystery".to_owned(),
            ..Default::default()
        };
        assert!(validate_ownership_problems(&problems).is_err());

        let model = OwnershipModel {
            schema_version: 14,
            status: "ready".to_owned(),
            repairs: vec![OwnershipRepair {
                id: "repair".to_owned(),
                title: "Repair".to_owned(),
                strategy: "borrow".to_owned(),
                semantics: "borrows the value".to_owned(),
                diff: "x".repeat(MAX_OWNERSHIP_TEXT_BYTES + 1),
                affected_files: Vec::new(),
                preview_complete: true,
                compiler_validated: false,
                validation_state: "candidate".to_owned(),
                effects: OwnershipRepairEffects::default(),
                preview_graph: None,
            }],
            ..Default::default()
        };
        assert!(validate_ownership_model(&model).is_err());
    }
}
