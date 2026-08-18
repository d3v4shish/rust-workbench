//! Book keeping for keeping diagnostics easily in sync with the client.
pub(crate) mod flycheck_to_proto;

use std::{mem, path::PathBuf};

use ide::FileId;
use ide_db::{FxHashMap, base_db::DbPanicContext};
use itertools::Itertools;
use rustc_hash::FxHashSet;
use smallvec::SmallVec;
use stdx::iter_eq_by;
use triomphe::Arc;

use crate::{
    flycheck::PackageSpecifier, global_state::GlobalStateSnapshot, lsp, lsp_ext,
    main_loop::DiagnosticsTaskKind,
};

pub(crate) type CheckFixes =
    Arc<Vec<FxHashMap<Option<PackageSpecifier>, FxHashMap<FileId, Vec<Fix>>>>>;
pub(crate) type OwnershipEvents =
    Arc<Vec<FxHashMap<Option<PackageSpecifier>, FxHashMap<FileId, Arc<Vec<OwnershipEvent>>>>>>;
pub(crate) type OwnershipDiagnostics =
    Arc<Vec<FxHashMap<Option<PackageSpecifier>, FxHashMap<FileId, Vec<OwnershipDiagnostic>>>>>;
pub(crate) type OwnershipTutorialModels = Arc<FxHashMap<FileId, Arc<OwnershipTutorialModel>>>;

#[derive(Default)]
pub(crate) struct RetiredOwnershipPayloads {
    pub(crate) event_sets: Vec<Arc<Vec<OwnershipEvent>>>,
    pub(crate) tutorial_models: Vec<Arc<OwnershipTutorialModel>>,
}

impl RetiredOwnershipPayloads {
    pub(crate) fn is_empty(&self) -> bool {
        self.event_sets.is_empty() && self.tutorial_models.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnershipDiagnostic {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) range: lsp_types::Range,
    pub(crate) related: Vec<OwnershipDiagnosticRelated>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnershipDiagnosticRelated {
    pub(crate) message: String,
    pub(crate) range: lsp_types::Range,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OwnershipEventKind {
    BorrowActivate,
    BorrowEnd,
    BorrowMutable,
    BorrowShared,
    Clone,
    Copy,
    Drop,
    InvalidUse,
    LastUse,
    Move,
    PartialMove,
    Reinitialize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OwnershipState {
    Available,
    Dropped,
    Moved,
    MutablyBorrowed,
    PartiallyMoved,
    SharedBorrowed,
}

impl OwnershipEventKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::BorrowActivate => "borrow activates",
            Self::BorrowEnd => "borrow ends",
            Self::BorrowMutable => "&mut borrow",
            Self::BorrowShared => "shared borrow",
            Self::Clone => "clone shared handle",
            Self::Copy => "copy",
            Self::Drop => "drop",
            Self::InvalidUse => "invalid use after move",
            Self::LastUse => "last use",
            Self::Move => "move",
            Self::PartialMove => "partial move",
            Self::Reinitialize => "reinitialized",
        }
    }

    pub(crate) fn modifier(self) -> &'static str {
        match self {
            Self::BorrowActivate => "borrowedMut",
            Self::BorrowEnd => "borrowEnd",
            Self::BorrowMutable => "borrowedMut",
            Self::BorrowShared => "borrowed",
            Self::Clone => "clone",
            Self::Copy => "copy",
            Self::Drop => "dropped",
            Self::InvalidUse => "invalidUse",
            Self::LastUse => "lastUse",
            Self::Move => "moved",
            Self::PartialMove => "partialMove",
            Self::Reinitialize => "reinitialized",
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipEventPayload {
    pub(crate) version: u8,
    pub(crate) kind: OwnershipEventKind,
    pub(crate) name: String,
    pub(crate) source_hash: String,
    pub(crate) binding_byte_start: usize,
    pub(crate) detail: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelPointer {
    pub(crate) version: u32,
    pub(crate) path: PathBuf,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelArtifact {
    pub(crate) schema_version: u32,
    #[serde(default)]
    pub(crate) target_triple: String,
    pub(crate) sources: Vec<OwnershipModelSource>,
    pub(crate) ownership_events: Vec<OwnershipModelEvent>,
    #[serde(default)]
    pub(crate) ownership_bodies: Vec<OwnershipModelBody>,
    #[serde(default)]
    pub(crate) ownership_bindings: Vec<OwnershipModelBindingFact>,
    #[serde(default)]
    pub(crate) ownership_loans: Vec<OwnershipModelLoan>,
    #[serde(default)]
    pub(crate) memory_graph: OwnershipModelMemoryGraph,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelSource {
    pub(crate) path: PathBuf,
    #[serde(default)]
    pub(crate) source_hash: String,
    #[serde(default)]
    pub(crate) source: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelBinding {
    pub(crate) path: PathBuf,
    pub(crate) byte_start: usize,
    pub(crate) name: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelSpan {
    pub(crate) path: PathBuf,
    pub(crate) byte_start: usize,
    pub(crate) byte_end: usize,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelBlock {
    pub(crate) basic_block: u32,
    pub(crate) span: OwnershipModelSpan,
    pub(crate) successors: Vec<u32>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelBody {
    pub(crate) body_id: u64,
    pub(crate) name: String,
    pub(crate) span: OwnershipModelSpan,
    pub(crate) blocks: Vec<OwnershipModelBlock>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelMemoryLayer {
    pub(crate) kind: String,
    pub(crate) storage: String,
    pub(crate) label: String,
    pub(crate) type_name: String,
    pub(crate) size: Option<u64>,
    pub(crate) align: Option<u64>,
    pub(crate) provenance: String,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub(crate) struct OwnershipModelMemoryGraph {
    #[serde(default)]
    pub(crate) nodes: Vec<OwnershipModelMemoryNode>,
    #[serde(default)]
    pub(crate) edges: Vec<OwnershipModelMemoryEdge>,
    #[serde(default)]
    pub(crate) snapshots: Vec<OwnershipModelSnapshot>,
    #[serde(default)]
    pub(crate) access_paths: Vec<OwnershipModelAccessPath>,
    #[serde(default)]
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelMemoryNode {
    pub(crate) id: String,
    pub(crate) body_id: u64,
    pub(crate) place: String,
    pub(crate) kind: String,
    pub(crate) storage: String,
    pub(crate) label: String,
    pub(crate) type_name: String,
    pub(crate) size: Option<u64>,
    pub(crate) align: Option<u64>,
    pub(crate) span: Option<OwnershipModelSpan>,
    pub(crate) state: OwnershipState,
    pub(crate) provenance: String,
    pub(crate) physical_placement_note: String,
    #[serde(default)]
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelMemoryEdge {
    pub(crate) id: String,
    pub(crate) source: String,
    pub(crate) target: String,
    pub(crate) relation: String,
    pub(crate) event_id: Option<String>,
    pub(crate) loan_id: Option<u32>,
    pub(crate) span: Option<OwnershipModelSpan>,
    pub(crate) provenance: String,
    pub(crate) path_marker: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelStateDelta {
    pub(crate) node_id: String,
    pub(crate) from: Option<OwnershipState>,
    pub(crate) to: OwnershipState,
    pub(crate) relation_added: Option<String>,
    pub(crate) relation_removed: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelSnapshot {
    pub(crate) id: String,
    pub(crate) event_id: String,
    pub(crate) body_id: u64,
    pub(crate) basic_block: u32,
    pub(crate) statement_index: u32,
    pub(crate) kind: String,
    pub(crate) span: OwnershipModelSpan,
    pub(crate) place: String,
    pub(crate) loan_id: Option<u32>,
    pub(crate) path_marker: Option<String>,
    pub(crate) deltas: Vec<OwnershipModelStateDelta>,
    pub(crate) provenance: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelAccessStep {
    pub(crate) kind: String,
    pub(crate) starting_type: String,
    pub(crate) result_type: String,
    pub(crate) mutability: String,
    pub(crate) explicitness: String,
    pub(crate) fallible: bool,
    pub(crate) may_panic: bool,
    pub(crate) requires_unsafe: bool,
    pub(crate) explanation: String,
    pub(crate) provenance: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelAccessPath {
    pub(crate) id: String,
    pub(crate) body_id: u64,
    pub(crate) node_id: String,
    pub(crate) place: String,
    pub(crate) purpose: String,
    pub(crate) steps: Vec<OwnershipModelAccessStep>,
    pub(crate) provenance: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelBindingFact {
    pub(crate) body_id: u64,
    pub(crate) binding: OwnershipModelBinding,
    pub(crate) type_name: String,
    pub(crate) size: Option<u64>,
    pub(crate) align: Option<u64>,
    pub(crate) memory_layers: Vec<OwnershipModelMemoryLayer>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelLoanPoint {
    pub(crate) basic_block: u32,
    pub(crate) statement_index: u32,
    pub(crate) span: OwnershipModelSpan,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelLoan {
    pub(crate) body_id: u64,
    pub(crate) loan_id: u32,
    pub(crate) kind: String,
    pub(crate) binding: OwnershipModelBinding,
    pub(crate) place: String,
    pub(crate) reserve: OwnershipModelLoanPoint,
    pub(crate) activation: Option<OwnershipModelLoanPoint>,
    pub(crate) live_points: Vec<OwnershipModelLoanPoint>,
    pub(crate) end_points: Vec<OwnershipModelLoanPoint>,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct OwnershipTutorialModel {
    pub(crate) schema_version: u32,
    pub(crate) target_triple: String,
    pub(crate) bodies: Vec<OwnershipTutorialBody>,
    pub(crate) bindings: Vec<OwnershipTutorialBinding>,
    pub(crate) loans: Vec<OwnershipTutorialLoan>,
    pub(crate) memory_graph: OwnershipTutorialMemoryGraph,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct OwnershipTutorialMemoryGraph {
    pub(crate) nodes: Vec<OwnershipTutorialMemoryNode>,
    pub(crate) edges: Vec<OwnershipTutorialMemoryEdge>,
    pub(crate) snapshots: Vec<OwnershipTutorialSnapshot>,
    pub(crate) access_paths: Vec<OwnershipModelAccessPath>,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct OwnershipTutorialMemoryEdge {
    pub(crate) edge: OwnershipModelMemoryEdge,
    pub(crate) range: Option<lsp_types::Range>,
}

#[derive(Clone, Debug)]
pub(crate) struct OwnershipTutorialMemoryNode {
    pub(crate) node: OwnershipModelMemoryNode,
    pub(crate) range: Option<lsp_types::Range>,
}

#[derive(Clone, Debug)]
pub(crate) struct OwnershipTutorialSnapshot {
    pub(crate) snapshot: OwnershipModelSnapshot,
    pub(crate) range: lsp_types::Range,
}

#[derive(Clone, Debug)]
pub(crate) struct OwnershipTutorialBody {
    pub(crate) body_id: u64,
    pub(crate) name: String,
    pub(crate) range: lsp_types::Range,
    pub(crate) blocks: Vec<OwnershipTutorialBlock>,
}

#[derive(Clone, Debug)]
pub(crate) struct OwnershipTutorialBlock {
    pub(crate) basic_block: u32,
    pub(crate) range: lsp_types::Range,
    pub(crate) successors: Vec<u32>,
}

#[derive(Clone, Debug)]
pub(crate) struct OwnershipTutorialBinding {
    pub(crate) body_id: u64,
    pub(crate) name: String,
    pub(crate) range: lsp_types::Range,
    pub(crate) type_name: String,
    pub(crate) size: Option<u64>,
    pub(crate) align: Option<u64>,
    pub(crate) memory_layers: Vec<OwnershipModelMemoryLayer>,
}

#[derive(Clone, Debug)]
pub(crate) struct OwnershipTutorialLoanPoint {
    pub(crate) basic_block: u32,
    pub(crate) statement_index: u32,
    pub(crate) range: lsp_types::Range,
}

#[derive(Clone, Debug)]
pub(crate) struct OwnershipTutorialLoan {
    pub(crate) body_id: u64,
    pub(crate) loan_id: u32,
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) place: String,
    pub(crate) reserve: OwnershipTutorialLoanPoint,
    pub(crate) activation: Option<OwnershipTutorialLoanPoint>,
    pub(crate) live_points: Vec<OwnershipTutorialLoanPoint>,
    pub(crate) end_points: Vec<OwnershipTutorialLoanPoint>,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelEvent {
    pub(crate) event_id: String,
    pub(crate) body_id: u64,
    pub(crate) basic_block: u32,
    pub(crate) statement_index: u32,
    pub(crate) kind: OwnershipEventKind,
    pub(crate) state: OwnershipState,
    pub(crate) path: PathBuf,
    pub(crate) byte_start: usize,
    pub(crate) byte_end: usize,
    pub(crate) binding: OwnershipModelBinding,
    pub(crate) place: String,
    pub(crate) loan_id: Option<u32>,
    pub(crate) detail: Option<String>,
    #[serde(default)]
    pub(crate) destination: Option<OwnershipModelDestination>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct OwnershipModelDestination {
    pub(crate) kind: String,
    pub(crate) label: String,
    pub(crate) place: Option<String>,
    pub(crate) span: Option<OwnershipModelSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnershipEvent {
    pub(crate) event_id: String,
    pub(crate) body_id: u64,
    pub(crate) basic_block: u32,
    pub(crate) statement_index: u32,
    pub(crate) kind: OwnershipEventKind,
    pub(crate) state: OwnershipState,
    pub(crate) range: lsp_types::Range,
    pub(crate) binding_range: lsp_types::Range,
    pub(crate) name: String,
    pub(crate) place: String,
    pub(crate) loan_id: Option<u32>,
    pub(crate) exact: bool,
    pub(crate) detail: Option<String>,
    pub(crate) destination: Option<OwnershipDestination>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnershipDestination {
    pub(crate) kind: String,
    pub(crate) label: String,
    pub(crate) place: Option<String>,
    pub(crate) range: Option<lsp_types::Range>,
}

pub(crate) fn stable_source_hash(source: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub(crate) fn ownership_events_for_file(
    events: &OwnershipEvents,
    file_id: FileId,
) -> Vec<OwnershipEvent> {
    let mut result = Vec::new();
    for event in events
        .iter()
        .flat_map(|flycheck| flycheck.values())
        .filter_map(|package| package.get(&file_id))
        .flat_map(|events| events.iter())
    {
        if !result.contains(event) {
            result.push(event.clone());
        }
    }
    result.sort_by_key(|event| (event.range.start, event.range.end));
    result
}

pub(crate) fn ownership_diagnostics_for_file(
    diagnostics: &OwnershipDiagnostics,
    file_id: FileId,
) -> Vec<OwnershipDiagnostic> {
    let mut result = Vec::new();
    for diagnostic in diagnostics
        .iter()
        .flat_map(|flycheck| flycheck.values())
        .filter_map(|package| package.get(&file_id))
        .flatten()
    {
        if !result.contains(diagnostic) {
            result.push(diagnostic.clone());
        }
    }
    result.sort_by_key(|diagnostic| (diagnostic.range.start, diagnostic.range.end));
    result
}

pub(crate) fn ownership_tutorial_for_file(
    models: &OwnershipTutorialModels,
    file_id: FileId,
) -> OwnershipTutorialModel {
    models.get(&file_id).map(|model| (**model).clone()).unwrap_or_default()
}

#[derive(Debug, Default, Clone)]
pub struct DiagnosticsMapConfig {
    pub remap_prefix: FxHashMap<String, String>,
    pub warnings_as_info: Vec<String>,
    pub warnings_as_hint: Vec<String>,
    pub check_ignore: FxHashSet<String>,
}

pub(crate) type DiagnosticsGeneration = usize;

#[derive(Debug, Clone, Default)]
pub(crate) struct WorkspaceFlycheckDiagnostic {
    pub(crate) per_package: FxHashMap<Option<PackageSpecifier>, PackageFlycheckDiagnostic>,
}

#[derive(Debug, Clone)]
pub(crate) struct PackageFlycheckDiagnostic {
    generation: DiagnosticsGeneration,
    per_file: FxHashMap<FileId, Vec<lsp_types::Diagnostic>>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct DiagnosticCollection {
    // FIXME: should be FxHashMap<FileId, Vec<ra_id::Diagnostic>>
    pub(crate) native_syntax:
        FxHashMap<FileId, (DiagnosticsGeneration, Vec<lsp_types::Diagnostic>)>,
    pub(crate) native_semantic:
        FxHashMap<FileId, (DiagnosticsGeneration, Vec<lsp_types::Diagnostic>)>,
    pub(crate) check: Vec<WorkspaceFlycheckDiagnostic>,
    pub(crate) check_fixes: CheckFixes,
    pub(crate) ownership_diagnostics: OwnershipDiagnostics,
    pub(crate) ownership_events: OwnershipEvents,
    pub(crate) ownership_tutorial_models: OwnershipTutorialModels,
    changes: FxHashSet<FileId>,
    /// Counter for supplying a new generation number for diagnostics.
    /// This is used to keep track of when to clear the diagnostics for a given file as we compute
    /// diagnostics on multiple worker threads simultaneously which may result in multiple diagnostics
    /// updates for the same file in a single generation update (due to macros affecting multiple files).
    generation: DiagnosticsGeneration,
}

#[derive(Clone)]
pub(crate) struct Fix {
    // Fixes may be triggerable from multiple ranges.
    pub(crate) ranges: SmallVec<[lsp_types::Range; 2]>,
    pub(crate) action: lsp_ext::CodeAction,
    pub(crate) ownership_wrapper: Option<OwnershipWrapperFix>,
}

impl std::fmt::Debug for Fix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Fix");
        debug.field("ranges", &self.ranges).field("action", &self.action);
        if let Some(ownership_wrapper) = self.ownership_wrapper {
            debug.field("ownership_wrapper", &ownership_wrapper);
        }
        debug.finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnershipWrapperFix {
    Arc,
    ArcMutex,
    ArcRwLock,
    Mutex,
    Rc,
    RefCell,
    RcRefCell,
    RwLock,
    All,
}

impl OwnershipWrapperFix {
    pub(crate) fn preview_order(self) -> u8 {
        match self {
            Self::Rc => 0,
            Self::Arc => 1,
            Self::RefCell => 2,
            Self::Mutex => 3,
            Self::RwLock => 4,
            Self::RcRefCell => 5,
            Self::ArcMutex => 6,
            Self::ArcRwLock => 7,
            Self::All => 8,
        }
    }

    pub(crate) fn runtime_semantics(self) -> &'static str {
        match self {
            Self::Rc => "single-threaded shared ownership; cloning increments a reference count",
            Self::Arc => {
                "thread-safe shared ownership; cloning increments an atomic reference count"
            }
            Self::RefCell => {
                "single-threaded interior mutability; conflicting borrows panic at runtime"
            }
            Self::Mutex => "exclusive synchronized mutation; access locks and can report poisoning",
            Self::RwLock => {
                "synchronized mutation with multiple readers or one writer; access can report poisoning"
            }
            Self::RcRefCell => {
                "single-threaded shared mutation; cloning shares runtime-checked interior state"
            }
            Self::ArcMutex => "thread-safe shared mutation; every access takes an exclusive lock",
            Self::ArcRwLock => {
                "thread-safe shared mutation; reads share a lock while writes are exclusive"
            }
            Self::All => "compiler-validated ownership-wrapper rewrite",
        }
    }
}

impl DiagnosticCollection {
    pub(crate) fn clear_check(&mut self, flycheck_id: usize) {
        let Some(check) = self.check.get_mut(flycheck_id) else {
            return;
        };
        self.changes.extend(check.per_package.drain().flat_map(|(_, v)| v.per_file.into_keys()));
        if let Some(fixes) = Arc::make_mut(&mut self.check_fixes).get_mut(flycheck_id) {
            fixes.clear();
        }
        if let Some(events) = Arc::make_mut(&mut self.ownership_events).get_mut(flycheck_id) {
            events.clear();
        }
        if let Some(diagnostics) =
            Arc::make_mut(&mut self.ownership_diagnostics).get_mut(flycheck_id)
        {
            diagnostics.clear();
        }
        // Tutorial facts are materialized per file rather than per flycheck. Clear them whenever a
        // compiler check is invalidated so the UI can never present facts from an older build.
        Arc::make_mut(&mut self.ownership_tutorial_models).clear();
    }

    pub(crate) fn clear_check_all(&mut self) {
        Arc::make_mut(&mut self.check_fixes).clear();
        Arc::make_mut(&mut self.ownership_diagnostics).clear();
        Arc::make_mut(&mut self.ownership_events).clear();
        Arc::make_mut(&mut self.ownership_tutorial_models).clear();
        self.changes.extend(
            self.check
                .iter_mut()
                .flat_map(|it| it.per_package.drain().flat_map(|(_, v)| v.per_file.into_keys())),
        )
    }

    pub(crate) fn clear_check_for_package(
        &mut self,
        flycheck_id: usize,
        package_id: PackageSpecifier,
    ) {
        let Some(check) = self.check.get_mut(flycheck_id) else {
            return;
        };
        let package_id = Some(package_id);
        if let Some(checks) = check.per_package.remove(&package_id) {
            self.changes.extend(checks.per_file.into_keys());
        }
        if let Some(fixes) = Arc::make_mut(&mut self.check_fixes).get_mut(flycheck_id) {
            fixes.remove(&package_id);
        }
        if let Some(events) = Arc::make_mut(&mut self.ownership_events).get_mut(flycheck_id) {
            events.remove(&package_id);
        }
        if let Some(diagnostics) =
            Arc::make_mut(&mut self.ownership_diagnostics).get_mut(flycheck_id)
        {
            diagnostics.remove(&package_id);
        }
        Arc::make_mut(&mut self.ownership_tutorial_models).clear();
    }

    pub(crate) fn clear_check_older_than(
        &mut self,
        flycheck_id: usize,
        generation: DiagnosticsGeneration,
    ) {
        if let Some(flycheck) = self.check.get_mut(flycheck_id) {
            let mut packages = vec![];
            self.changes.extend(
                flycheck
                    .per_package
                    .extract_if(|_, v| v.generation < generation)
                    .inspect(|(package_id, _)| packages.push(package_id.clone()))
                    .flat_map(|(_, v)| v.per_file.into_keys()),
            );
            if let Some(fixes) = Arc::make_mut(&mut self.check_fixes).get_mut(flycheck_id) {
                for package in &packages {
                    fixes.remove(package);
                }
            }
            if let Some(events) = Arc::make_mut(&mut self.ownership_events).get_mut(flycheck_id) {
                for package in &packages {
                    events.remove(package);
                }
            }
            if let Some(diagnostics) =
                Arc::make_mut(&mut self.ownership_diagnostics).get_mut(flycheck_id)
            {
                for package in &packages {
                    diagnostics.remove(package);
                }
            }
            if !packages.is_empty() {
                Arc::make_mut(&mut self.ownership_tutorial_models).clear();
            }
        }
    }

    pub(crate) fn clear_check_older_than_for_package(
        &mut self,
        flycheck_id: usize,
        package_id: PackageSpecifier,
        generation: DiagnosticsGeneration,
    ) {
        let Some(check) = self.check.get_mut(flycheck_id) else {
            return;
        };
        let package_id = Some(package_id);
        let Some((_, checks)) = check
            .per_package
            .extract_if(|k, v| *k == package_id && v.generation < generation)
            .next()
        else {
            return;
        };
        self.changes.extend(checks.per_file.into_keys());
        if let Some(fixes) = Arc::make_mut(&mut self.check_fixes).get_mut(flycheck_id) {
            fixes.remove(&package_id);
        }
        if let Some(events) = Arc::make_mut(&mut self.ownership_events).get_mut(flycheck_id) {
            events.remove(&package_id);
        }
        if let Some(diagnostics) =
            Arc::make_mut(&mut self.ownership_diagnostics).get_mut(flycheck_id)
        {
            diagnostics.remove(&package_id);
        }
        Arc::make_mut(&mut self.ownership_tutorial_models).clear();
    }

    pub(crate) fn clear_native_for(&mut self, file_id: FileId) {
        self.native_syntax.remove(&file_id);
        self.native_semantic.remove(&file_id);
        self.changes.insert(file_id);
    }

    pub(crate) fn clear_ownership_for(&mut self, file_id: FileId) {
        for flycheck in Arc::make_mut(&mut self.ownership_diagnostics) {
            for package in flycheck.values_mut() {
                package.remove(&file_id);
            }
        }
        for flycheck in Arc::make_mut(&mut self.ownership_events) {
            for package in flycheck.values_mut() {
                package.remove(&file_id);
            }
        }
        Arc::make_mut(&mut self.ownership_tutorial_models).remove(&file_id);
        self.changes.insert(file_id);
    }

    pub(crate) fn replace_exact_ownership_events(
        &mut self,
        flycheck_id: usize,
        package_id: &Option<PackageSpecifier>,
        file_id: FileId,
        new_events: Vec<OwnershipEvent>,
    ) -> RetiredOwnershipPayloads {
        let mut retired = RetiredOwnershipPayloads::default();
        let ownership_events = Arc::make_mut(&mut self.ownership_events);
        for flycheck in ownership_events.iter_mut() {
            for package in flycheck.values_mut() {
                if let Some(events) = package.get_mut(&file_id) {
                    if events.iter().any(|event| event.exact) {
                        let previous = std::mem::take(events);
                        *events = Arc::new(
                            previous.iter().filter(|event| !event.exact).cloned().collect(),
                        );
                        retired.event_sets.push(previous);
                    }
                }
            }
        }
        if ownership_events.len() <= flycheck_id {
            ownership_events.resize_with(flycheck_id + 1, Default::default);
        }
        let events = ownership_events[flycheck_id]
            .entry(package_id.clone())
            .or_default()
            .entry(file_id)
            .or_default();
        let estimated = std::mem::take(events);
        let mut replacement = Vec::with_capacity(estimated.len() + new_events.len());
        replacement.extend(estimated.iter().cloned());
        replacement.extend(new_events);
        replacement.sort_by_key(|event| (event.range.start, event.range.end));
        replacement.dedup();
        *events = Arc::new(replacement);
        retired.event_sets.push(estimated);
        if let Some(model) = Arc::make_mut(&mut self.ownership_tutorial_models).remove(&file_id) {
            retired.tutorial_models.push(model);
        }
        self.changes.insert(file_id);
        retired
    }

    pub(crate) fn set_ownership_tutorial_model(
        &mut self,
        file_id: FileId,
        model: OwnershipTutorialModel,
    ) -> Option<Arc<OwnershipTutorialModel>> {
        let retired =
            Arc::make_mut(&mut self.ownership_tutorial_models).insert(file_id, Arc::new(model));
        self.changes.insert(file_id);
        retired
    }

    pub(crate) fn add_check_diagnostic(
        &mut self,
        flycheck_id: usize,
        generation: DiagnosticsGeneration,
        package_id: &Option<PackageSpecifier>,
        file_id: FileId,
        diagnostic: lsp_types::Diagnostic,
        fix: Option<Box<Fix>>,
    ) {
        if self.check.len() <= flycheck_id {
            self.check.resize_with(flycheck_id + 1, WorkspaceFlycheckDiagnostic::default);
        }

        let check = &mut self.check[flycheck_id];
        let package = check.per_package.entry(package_id.clone()).or_insert_with(|| {
            PackageFlycheckDiagnostic { generation, per_file: FxHashMap::default() }
        });
        // Getting message from old generation. Might happen in restarting checks.
        if package.generation > generation {
            return;
        }
        package.generation = generation;
        let diagnostics = package.per_file.entry(file_id).or_default();
        for existing_diagnostic in diagnostics.iter() {
            if are_diagnostics_equal(existing_diagnostic, &diagnostic) {
                return;
            }
        }

        if let Some(mut fix) = fix {
            let check_fixes = Arc::make_mut(&mut self.check_fixes);
            if check_fixes.len() <= flycheck_id {
                check_fixes.resize_with(flycheck_id + 1, Default::default);
            }
            fix.ranges.push(diagnostic.range);
            check_fixes[flycheck_id]
                .entry(package_id.clone())
                .or_default()
                .entry(file_id)
                .or_default()
                .push(*fix);
        }
        diagnostics.push(diagnostic);
        self.changes.insert(file_id);
    }

    pub(crate) fn add_ownership_event(
        &mut self,
        flycheck_id: usize,
        package_id: &Option<PackageSpecifier>,
        file_id: FileId,
        event: OwnershipEvent,
    ) {
        let ownership_events = Arc::make_mut(&mut self.ownership_events);
        if ownership_events.len() <= flycheck_id {
            ownership_events.resize_with(flycheck_id + 1, Default::default);
        }
        let events = ownership_events[flycheck_id]
            .entry(package_id.clone())
            .or_default()
            .entry(file_id)
            .or_default();
        let events = Arc::make_mut(events);
        if !events.contains(&event) {
            events.push(event);
            events.sort_by_key(|event| (event.range.start, event.range.end));
        }
    }

    pub(crate) fn add_ownership_diagnostic(
        &mut self,
        flycheck_id: usize,
        package_id: &Option<PackageSpecifier>,
        file_id: FileId,
        diagnostic: OwnershipDiagnostic,
    ) {
        let diagnostics = Arc::make_mut(&mut self.ownership_diagnostics);
        if diagnostics.len() <= flycheck_id {
            diagnostics.resize_with(flycheck_id + 1, Default::default);
        }
        let diagnostics = diagnostics[flycheck_id]
            .entry(package_id.clone())
            .or_default()
            .entry(file_id)
            .or_default();
        if !diagnostics.contains(&diagnostic) {
            diagnostics.push(diagnostic);
            diagnostics.sort_by_key(|diagnostic| (diagnostic.range.start, diagnostic.range.end));
        }
    }

    pub(crate) fn set_native_diagnostics(&mut self, kind: DiagnosticsTaskKind) {
        let (generation, diagnostics, target) = match kind {
            DiagnosticsTaskKind::Syntax(generation, diagnostics) => {
                (generation, diagnostics, &mut self.native_syntax)
            }
            DiagnosticsTaskKind::Semantic(generation, diagnostics) => {
                (generation, diagnostics, &mut self.native_semantic)
            }
        };

        for (file_id, mut diagnostics) in diagnostics {
            diagnostics.sort_by_key(|it| (it.range.start, it.range.end));

            if let Some((old_gen, existing_diagnostics)) = target.get_mut(&file_id) {
                if existing_diagnostics.len() == diagnostics.len()
                    && iter_eq_by(&diagnostics, &*existing_diagnostics, |new, existing| {
                        are_diagnostics_equal(new, existing)
                    })
                {
                    // don't signal an update if the diagnostics are the same
                    continue;
                }
                if *old_gen < generation || generation == 0 {
                    target.insert(file_id, (generation, diagnostics));
                } else {
                    existing_diagnostics.extend(diagnostics);
                    // FIXME: Doing the merge step of a merge sort here would be a bit more performant
                    // but eh
                    existing_diagnostics.sort_by_key(|it| (it.range.start, it.range.end))
                }
            } else {
                target.insert(file_id, (generation, diagnostics));
            }
            self.changes.insert(file_id);
        }
    }

    pub(crate) fn diagnostics_for(
        &self,
        file_id: FileId,
    ) -> impl Iterator<Item = &lsp_types::Diagnostic> {
        let native_syntax = self.native_syntax.get(&file_id).into_iter().flat_map(|(_, d)| d);
        let native_semantic = self.native_semantic.get(&file_id).into_iter().flat_map(|(_, d)| d);
        let check = self
            .check
            .iter()
            .flat_map(|it| it.per_package.values())
            .filter_map(move |it| it.per_file.get(&file_id))
            .flatten();
        native_syntax.chain(native_semantic).chain(check)
    }

    pub(crate) fn take_changes(&mut self) -> Option<FxHashSet<FileId>> {
        if self.changes.is_empty() {
            return None;
        }
        Some(mem::take(&mut self.changes))
    }

    pub(crate) fn next_generation(&mut self) -> usize {
        self.generation += 1;
        self.generation
    }
}

fn are_diagnostics_equal(left: &lsp_types::Diagnostic, right: &lsp_types::Diagnostic) -> bool {
    left.source == right.source
        && left.severity == right.severity
        && left.range == right.range
        && left.message == right.message
}

pub(crate) enum NativeDiagnosticsFetchKind {
    Syntax,
    Semantic,
}

pub(crate) fn fetch_native_diagnostics(
    snapshot: &GlobalStateSnapshot,
    subscriptions: std::sync::Arc<[FileId]>,
    slice: std::ops::Range<usize>,
    kind: NativeDiagnosticsFetchKind,
) -> Vec<(FileId, Vec<lsp_types::Diagnostic>)> {
    let _p = tracing::info_span!("fetch_native_diagnostics").entered();
    let _ctx = DbPanicContext::enter("fetch_native_diagnostics".to_owned());

    // the diagnostics produced may point to different files not requested by the concrete request,
    // put those into here and filter later
    let mut odd_ones = Vec::new();
    let mut diagnostics = subscriptions[slice]
        .iter()
        .copied()
        .map(|file_id| {
            let diagnostics = (|| {
                let line_index = snapshot.file_line_index(file_id).ok()?;
                let source_root = snapshot.analysis.source_root_id(file_id).ok()?;

                let config = &snapshot.config.diagnostics(Some(source_root));
                let diagnostics = match kind {
                    NativeDiagnosticsFetchKind::Syntax => {
                        snapshot.analysis.syntax_diagnostics(config, file_id).ok()?
                    }

                    NativeDiagnosticsFetchKind::Semantic if config.enabled => snapshot
                        .analysis
                        .semantic_diagnostics(config, ide::AssistResolveStrategy::None, file_id)
                        .ok()?,
                    NativeDiagnosticsFetchKind::Semantic => return None,
                };
                Some(
                    diagnostics
                        .into_iter()
                        .filter_map(|d| {
                            if d.range.file_id == file_id {
                                Some(convert_diagnostic(&line_index, d))
                            } else {
                                odd_ones.push(d);
                                None
                            }
                        })
                        .collect::<Vec<_>>(),
                )
            })()
            .unwrap_or_default();

            (file_id, diagnostics)
        })
        .collect::<Vec<_>>();

    // Add back any diagnostics that point to files we are subscribed to
    for (file_id, group) in odd_ones
        .into_iter()
        .sorted_by_key(|it| it.range.file_id)
        .chunk_by(|it| it.range.file_id)
        .into_iter()
    {
        if !subscriptions.contains(&file_id) {
            continue;
        }
        let Some((_, diagnostics)) = diagnostics.iter_mut().find(|&&mut (id, _)| id == file_id)
        else {
            continue;
        };
        let Some(line_index) = snapshot.file_line_index(file_id).ok() else {
            break;
        };
        for diagnostic in group {
            diagnostics.push(convert_diagnostic(&line_index, diagnostic));
        }
    }
    diagnostics
}

pub(crate) fn convert_diagnostic(
    line_index: &crate::line_index::LineIndex,
    d: ide::Diagnostic,
) -> lsp_types::Diagnostic {
    lsp_types::Diagnostic {
        range: lsp::to_proto::range(line_index, d.range.range),
        severity: Some(lsp::to_proto::diagnostic_severity(d.severity)),
        code: Some(lsp_types::Code::String(d.code.as_str().to_owned())),
        code_description: Some(lsp_types::CodeDescription {
            href: lsp_types::Uri::parse(&d.code.url()).unwrap(),
        }),
        source: Some("rust-analyzer".to_owned()),
        message: lsp_types::Message::String(d.message),
        related_information: None,
        tags: d.unused.then(|| vec![lsp_types::DiagnosticTag::Unnecessary]),
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_a_file_clears_stale_ownership_events() {
        let mut diagnostics = DiagnosticCollection::default();
        let file_id = FileId::from_raw(0);
        diagnostics.add_ownership_event(
            0,
            &None,
            file_id,
            OwnershipEvent {
                event_id: "event".to_owned(),
                body_id: 1,
                basic_block: 0,
                statement_index: 0,
                kind: OwnershipEventKind::Move,
                state: OwnershipState::Moved,
                range: lsp_types::Range::default(),
                binding_range: lsp_types::Range::default(),
                name: "value".to_owned(),
                place: "value".to_owned(),
                loan_id: None,
                exact: true,
                detail: None,
                destination: None,
            },
        );

        assert_eq!(ownership_events_for_file(&diagnostics.ownership_events, file_id).len(), 1);
        diagnostics.clear_ownership_for(file_id);
        assert!(ownership_events_for_file(&diagnostics.ownership_events, file_id).is_empty());
    }

    #[test]
    fn replacing_cached_ownership_keeps_only_estimated_events() {
        let mut diagnostics = DiagnosticCollection::default();
        let file_id = FileId::from_raw(0);
        let event = OwnershipEvent {
            event_id: "exact".to_owned(),
            body_id: 1,
            basic_block: 0,
            statement_index: 0,
            kind: OwnershipEventKind::Move,
            state: OwnershipState::Moved,
            range: lsp_types::Range::default(),
            binding_range: lsp_types::Range::default(),
            name: "value".to_owned(),
            place: "value".to_owned(),
            loan_id: None,
            exact: true,
            detail: None,
            destination: None,
        };
        diagnostics.add_ownership_event(0, &None, file_id, event.clone());
        diagnostics.add_ownership_event(
            0,
            &None,
            file_id,
            OwnershipEvent { event_id: "estimated".to_owned(), exact: false, ..event },
        );

        drop(diagnostics.replace_exact_ownership_events(0, &None, file_id, Vec::new()));

        let events = ownership_events_for_file(&diagnostics.ownership_events, file_id);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_id, "estimated");
        assert!(!events[0].exact);
    }

    #[test]
    fn editing_a_file_clears_stale_ownership_diagnostics() {
        let mut diagnostics = DiagnosticCollection::default();
        let file_id = FileId::from_raw(0);
        diagnostics.add_ownership_diagnostic(
            0,
            &None,
            file_id,
            OwnershipDiagnostic {
                code: "E0382".to_owned(),
                message: "borrow of moved value: `value`".to_owned(),
                range: lsp_types::Range::default(),
                related: Vec::new(),
            },
        );
        assert_eq!(
            ownership_diagnostics_for_file(&diagnostics.ownership_diagnostics, file_id).len(),
            1
        );
        diagnostics.clear_ownership_for(file_id);
        assert!(
            ownership_diagnostics_for_file(&diagnostics.ownership_diagnostics, file_id).is_empty()
        );
    }

    #[test]
    fn bulk_ownership_artifact_commit_is_linear_enough_for_the_event_loop() {
        let mut diagnostics = DiagnosticCollection::default();
        let file_id = FileId::from_raw(0);
        let range = lsp_types::Range::default();
        diagnostics.add_ownership_event(
            0,
            &None,
            file_id,
            OwnershipEvent {
                event_id: "estimated".to_owned(),
                body_id: 1,
                basic_block: 0,
                statement_index: 0,
                kind: OwnershipEventKind::Move,
                state: OwnershipState::Moved,
                range,
                binding_range: range,
                name: "value".to_owned(),
                place: "value".to_owned(),
                loan_id: None,
                exact: false,
                detail: None,
                destination: None,
            },
        );
        let events = (0..4096)
            .map(|index| {
                let position = lsp_types::Position::new(index / 128, index % 128);
                let range = lsp_types::Range::new(position, position);
                OwnershipEvent {
                    event_id: format!("exact-{index}"),
                    body_id: 1,
                    basic_block: index / 128,
                    statement_index: index % 128,
                    kind: OwnershipEventKind::LastUse,
                    state: OwnershipState::Available,
                    range,
                    binding_range: range,
                    name: "value".to_owned(),
                    place: "value".to_owned(),
                    loan_id: None,
                    exact: true,
                    detail: None,
                    destination: None,
                }
            })
            .collect::<Vec<_>>();
        drop(diagnostics.replace_exact_ownership_events(0, &None, file_id, events.clone()));
        let active_request_snapshot = Arc::clone(&diagnostics.ownership_events);
        let started = std::time::Instant::now();
        drop(diagnostics.replace_exact_ownership_events(0, &None, file_id, events));
        let elapsed = started.elapsed();
        let committed = ownership_events_for_file(&diagnostics.ownership_events, file_id);
        assert_eq!(committed.len(), 4097);
        assert!(committed.iter().any(|event| event.event_id == "estimated"));
        assert_eq!(ownership_events_for_file(&active_request_snapshot, file_id).len(), 4097);
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "bulk ownership commit took {elapsed:?}"
        );
    }
}
