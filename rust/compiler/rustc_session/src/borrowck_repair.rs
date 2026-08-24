//! Structured source edits produced by conservative borrow-checker diagnostics.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io;
use std::path::PathBuf;

use rustc_data_structures::sync::Lock;
use rustc_hir::HirId;
use rustc_hir::def_id::LocalDefId;
use rustc_span::{FileName, Pos, Span};
use serde::{Deserialize, Serialize};

use crate::Session;

pub const AUTOFIX_CHILD_ENV: &str = "RUSTC_BORROWCK_AUTOFIX_CHILD";
pub const AUTOFIX_OVERLAY_ENV: &str = "RUSTC_BORROWCK_AUTOFIX_OVERLAY";
pub const AUTOFIX_PLAN_ENV: &str = "RUSTC_BORROWCK_AUTOFIX_PLAN";
pub const BORROWCK_OWNERSHIP_MODEL_SCHEMA_VERSION: u32 = 7;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckRepairKind {
    CloneMovedValue,
    MakeBindingMutable,
}

#[derive(Clone, Debug)]
pub struct BorrowckRepairEdit {
    pub span: Span,
    pub replacement: String,
}

#[derive(Clone, Debug)]
pub struct BorrowckRepairGroup {
    pub kind: BorrowckRepairKind,
    pub binding_span: Option<Span>,
    pub edits: Vec<BorrowckRepairEdit>,
}

#[derive(Default)]
pub struct BorrowckRepairCollector {
    pub ownership_bindings: Lock<Vec<BorrowckOwnershipBinding>>,
    pub ownership_bodies: Lock<Vec<BorrowckOwnershipBody>>,
    pub ownership_events: Lock<Vec<BorrowckOwnershipEvent>>,
    pub ownership_loans: Lock<Vec<BorrowckOwnershipLoan>>,
    pub repairs: Lock<Vec<BorrowckRepairGroup>>,
    pub wrapper_intents: Lock<Vec<BorrowckWrapperIntent>>,
    pub wrapper_variants: Lock<Vec<BorrowckWrapperVariant>>,
    pub wrapper_rejections: Lock<Vec<BorrowckWrapperRejection>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckMemoryKind {
    AggregateValue,
    ArcAllocation,
    ArcHandle,
    BoxAllocation,
    BoxHandle,
    CellState,
    ConditionalValue,
    ContainerBuffer,
    ContainerHeader,
    FatPointerMetadata,
    GuardState,
    InlineValue,
    MutexState,
    OnceState,
    PinConstraint,
    RawPointer,
    RcAllocation,
    RcHandle,
    ReferenceHandle,
    RefCellState,
    RwLockState,
    StackBinding,
    StringBuffer,
    StringHeader,
    UnsafeCellState,
    VecBuffer,
    VecHeader,
    WeakAllocation,
    WeakHandle,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckMemoryStorage {
    Conceptual,
    Heap,
    Inline,
    Stack,
}

#[derive(Clone, Debug)]
pub struct BorrowckMemoryLayer {
    pub kind: BorrowckMemoryKind,
    pub storage: BorrowckMemoryStorage,
    pub label: String,
    pub type_name: String,
    pub size: Option<u64>,
    pub align: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct BorrowckOwnershipBinding {
    pub body_id: u64,
    pub span: Span,
    pub name: String,
    pub type_name: String,
    pub size: Option<u64>,
    pub align: Option<u64>,
    pub memory_layers: Vec<BorrowckMemoryLayer>,
}

#[derive(Clone, Debug)]
pub struct BorrowckOwnershipBlock {
    pub basic_block: u32,
    pub span: Span,
    pub successors: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct BorrowckOwnershipBody {
    pub body_id: u64,
    pub name: String,
    pub span: Span,
    pub blocks: Vec<BorrowckOwnershipBlock>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckLoanKind {
    Mutable,
    Shared,
}

#[derive(Clone, Debug)]
pub struct BorrowckOwnershipLoanPoint {
    pub basic_block: u32,
    pub statement_index: u32,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct BorrowckOwnershipLoan {
    pub body_id: u64,
    pub loan_id: u32,
    pub kind: BorrowckLoanKind,
    pub binding_span: Span,
    pub binding_name: String,
    pub place: String,
    pub reserve: BorrowckOwnershipLoanPoint,
    pub activation: Option<BorrowckOwnershipLoanPoint>,
    pub live_points: Vec<BorrowckOwnershipLoanPoint>,
    pub end_points: Vec<BorrowckOwnershipLoanPoint>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckOwnershipEventKind {
    BorrowActivate,
    BorrowEnd,
    BorrowMutable,
    BorrowShared,
    Clone,
    Copy,
    Drop,
    LastUse,
    Move,
    PartialMove,
    Reinitialize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckOwnershipDestinationKind {
    FunctionArgument,
    LocalBinding,
    ProjectedPlace,
    ReturnValue,
}

#[derive(Clone, Debug)]
pub struct BorrowckOwnershipDestination {
    pub kind: BorrowckOwnershipDestinationKind,
    pub label: String,
    pub place: Option<String>,
    pub span: Option<Span>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckOwnershipState {
    Available,
    Dropped,
    Moved,
    MutablyBorrowed,
    PartiallyMoved,
    SharedBorrowed,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug)]
pub struct BorrowckOwnershipEvent {
    pub body_id: u64,
    pub basic_block: u32,
    pub statement_index: u32,
    pub kind: BorrowckOwnershipEventKind,
    pub state: BorrowckOwnershipState,
    pub span: Span,
    pub binding_span: Span,
    pub binding_name: String,
    pub place: String,
    pub loan_id: Option<u32>,
    pub detail: Option<String>,
    pub destination: Option<BorrowckOwnershipDestination>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BorrowckWrapperSource {
    Plain,
    Box,
    Rc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BorrowckWrapperRequirement {
    SharedOwnership,
    InteriorMutation,
}

#[derive(Clone, Debug)]
pub struct BorrowckWrapperIntent {
    pub body_owner: LocalDefId,
    pub binding_hir_id: HirId,
    pub binding_span: Span,
    pub trigger_span: Span,
    pub binding_name: String,
    pub source: BorrowckWrapperSource,
    pub requirement: BorrowckWrapperRequirement,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckWrapperStrategy {
    Arc,
    ArcMutex,
    ArcRwLock,
    Mutex,
    Rc,
    RefCell,
    RcRefCell,
    RwLock,
}

#[derive(Clone, Debug)]
pub struct BorrowckWrapperVariant {
    pub binding_span: Span,
    pub trigger_span: Span,
    pub binding_name: String,
    pub strategy: BorrowckWrapperStrategy,
    pub edits: Vec<BorrowckRepairEdit>,
}

#[derive(Clone, Debug)]
pub struct BorrowckWrapperRejection {
    pub binding_span: Span,
    pub binding_name: String,
    pub strategy: BorrowckWrapperStrategy,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckRepairSource {
    pub path: PathBuf,
    pub source_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckRepairEdit {
    pub path: PathBuf,
    pub byte_start: usize,
    pub byte_end: usize,
    pub replacement: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SerializedBorrowckBinding {
    pub path: PathBuf,
    pub byte_start: usize,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckRepairGroup {
    pub kind: BorrowckRepairKind,
    pub binding: Option<SerializedBorrowckBinding>,
    pub edits: Vec<SerializedBorrowckRepairEdit>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckWrapperVariant {
    pub binding: SerializedBorrowckBinding,
    pub trigger: SerializedBorrowckSpan,
    pub strategy: BorrowckWrapperStrategy,
    pub edits: Vec<SerializedBorrowckRepairEdit>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SerializedBorrowckSpan {
    pub path: PathBuf,
    pub byte_start: usize,
    pub byte_end: usize,
}

/// Where a source-level value is represented. This describes Rust's abstract
/// storage model; it deliberately does not promise where an optimizer will
/// place a value in the final machine code.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckGraphStorageRegion {
    Stack,
    Inline,
    Heap,
    Static,
    ThreadLocal,
    Conceptual,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckGraphNodeKind {
    Binding,
    Handle,
    Wrapper,
    InlineValue,
    HeapAllocation,
    Buffer,
    ControlBlock,
    BorrowFlag,
    LockState,
    BorrowedView,
    ProjectedPlace,
    Guard,
    Metadata,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckGraphEdgeRelation {
    Stores,
    Wraps,
    Owns,
    Contains,
    OwnsBuffer,
    PointsTo,
    BorrowShared,
    BorrowMutable,
    Reborrow,
    SharesAllocation,
    WeakReference,
    GuardsAccess,
    Conditional,
    MovedTo,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckOwnershipProvenance {
    Exact,
    Derived,
    Conceptual,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckSnapshotKind {
    Initialize,
    BorrowReserve,
    BorrowActivate,
    Move,
    Copy,
    Clone,
    Reborrow,
    LastUse,
    BorrowEnd,
    Conflict,
    Reinitialize,
    Drop,
    PartialMove,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckAccessKind {
    BuiltinDeref,
    TraitDeref,
    TraitDerefMut,
    AutoBorrowShared,
    AutoBorrowMutable,
    RawPointerDeref,
    WrapperBorrow,
    WrapperBorrowMut,
    GuardDeref,
    WeakUpgrade,
    OptionExtract,
    ResultExtract,
    LockAcquire,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckAccessMutability {
    Shared,
    Mutable,
    NotApplicable,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowckAccessExplicitness {
    Automatic,
    Explicit,
    Either,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckMemoryNode {
    pub id: String,
    pub body_id: u64,
    pub place: String,
    pub kind: BorrowckGraphNodeKind,
    pub storage: BorrowckGraphStorageRegion,
    pub label: String,
    pub type_name: String,
    pub size: Option<u64>,
    pub align: Option<u64>,
    pub span: Option<SerializedBorrowckSpan>,
    pub state: BorrowckOwnershipState,
    pub provenance: BorrowckOwnershipProvenance,
    pub physical_placement_note: String,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckMemoryEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub relation: BorrowckGraphEdgeRelation,
    pub event_id: Option<String>,
    pub loan_id: Option<u32>,
    pub span: Option<SerializedBorrowckSpan>,
    pub provenance: BorrowckOwnershipProvenance,
    pub path_marker: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckStateDelta {
    pub node_id: String,
    pub from: Option<BorrowckOwnershipState>,
    pub to: BorrowckOwnershipState,
    pub relation_added: Option<BorrowckGraphEdgeRelation>,
    pub relation_removed: Option<BorrowckGraphEdgeRelation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckOwnershipSnapshot {
    pub id: String,
    pub event_id: String,
    pub body_id: u64,
    pub basic_block: u32,
    pub statement_index: u32,
    pub kind: BorrowckSnapshotKind,
    pub span: SerializedBorrowckSpan,
    pub place: String,
    pub loan_id: Option<u32>,
    pub path_marker: Option<String>,
    pub deltas: Vec<SerializedBorrowckStateDelta>,
    pub provenance: BorrowckOwnershipProvenance,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckAccessStep {
    pub kind: BorrowckAccessKind,
    pub starting_type: String,
    pub result_type: String,
    pub mutability: BorrowckAccessMutability,
    pub explicitness: BorrowckAccessExplicitness,
    pub fallible: bool,
    pub may_panic: bool,
    pub requires_unsafe: bool,
    pub explanation: String,
    pub provenance: BorrowckOwnershipProvenance,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckAccessPath {
    pub id: String,
    pub body_id: u64,
    pub node_id: String,
    pub place: String,
    pub purpose: String,
    pub steps: Vec<SerializedBorrowckAccessStep>,
    pub provenance: BorrowckOwnershipProvenance,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SerializedBorrowckMemoryGraph {
    #[serde(default)]
    pub nodes: Vec<SerializedBorrowckMemoryNode>,
    #[serde(default)]
    pub edges: Vec<SerializedBorrowckMemoryEdge>,
    #[serde(default)]
    pub snapshots: Vec<SerializedBorrowckOwnershipSnapshot>,
    #[serde(default)]
    pub access_paths: Vec<SerializedBorrowckAccessPath>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckMemoryLayer {
    pub kind: BorrowckMemoryKind,
    pub storage: BorrowckMemoryStorage,
    pub label: String,
    pub type_name: String,
    pub size: Option<u64>,
    pub align: Option<u64>,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckOwnershipBinding {
    pub body_id: u64,
    pub binding: SerializedBorrowckBinding,
    pub type_name: String,
    pub size: Option<u64>,
    pub align: Option<u64>,
    pub memory_layers: Vec<SerializedBorrowckMemoryLayer>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckOwnershipBlock {
    pub basic_block: u32,
    pub span: SerializedBorrowckSpan,
    pub successors: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckOwnershipBody {
    pub body_id: u64,
    pub name: String,
    pub span: SerializedBorrowckSpan,
    pub blocks: Vec<SerializedBorrowckOwnershipBlock>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckOwnershipLoanPoint {
    pub basic_block: u32,
    pub statement_index: u32,
    pub span: SerializedBorrowckSpan,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckOwnershipLoan {
    pub body_id: u64,
    pub loan_id: u32,
    pub kind: BorrowckLoanKind,
    pub binding: SerializedBorrowckBinding,
    pub place: String,
    pub reserve: SerializedBorrowckOwnershipLoanPoint,
    pub activation: Option<SerializedBorrowckOwnershipLoanPoint>,
    pub live_points: Vec<SerializedBorrowckOwnershipLoanPoint>,
    pub end_points: Vec<SerializedBorrowckOwnershipLoanPoint>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckWrapperRejection {
    pub binding: SerializedBorrowckBinding,
    pub strategy: BorrowckWrapperStrategy,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckOwnershipEvent {
    pub event_id: String,
    pub body_id: u64,
    pub basic_block: u32,
    pub statement_index: u32,
    pub kind: BorrowckOwnershipEventKind,
    pub state: BorrowckOwnershipState,
    pub path: PathBuf,
    pub byte_start: usize,
    pub byte_end: usize,
    pub binding: SerializedBorrowckBinding,
    pub place: String,
    pub loan_id: Option<u32>,
    pub detail: Option<String>,
    #[serde(default)]
    pub destination: Option<SerializedBorrowckOwnershipDestination>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckOwnershipDestination {
    pub kind: BorrowckOwnershipDestinationKind,
    pub label: String,
    pub place: Option<String>,
    pub span: Option<SerializedBorrowckSpan>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SerializedBorrowckRepairPlan {
    pub schema_version: u32,
    #[serde(default)]
    pub target_triple: String,
    pub crate_name: String,
    pub stable_crate_id: u64,
    pub sources: Vec<SerializedBorrowckRepairSource>,
    pub ownership_bodies: Vec<SerializedBorrowckOwnershipBody>,
    pub ownership_bindings: Vec<SerializedBorrowckOwnershipBinding>,
    pub ownership_events: Vec<SerializedBorrowckOwnershipEvent>,
    pub ownership_loans: Vec<SerializedBorrowckOwnershipLoan>,
    #[serde(default)]
    pub memory_graph: SerializedBorrowckMemoryGraph,
    pub repairs: Vec<SerializedBorrowckRepairGroup>,
    pub wrapper_variants: Vec<SerializedBorrowckWrapperVariant>,
    pub wrapper_rejections: Vec<SerializedBorrowckWrapperRejection>,
}

impl Session {
    fn borrowck_ownership_model_enabled(&self) -> bool {
        self.opts.unstable_opts.borrowck_ownership_events
            || self.opts.unstable_opts.borrowck_ownership_model.is_some()
    }

    pub fn record_borrowck_ownership_body(&self, body: BorrowckOwnershipBody) {
        if self.borrowck_ownership_model_enabled() {
            self.borrowck_repairs.ownership_bodies.lock().push(body);
        }
    }

    pub fn record_borrowck_ownership_binding(&self, binding: BorrowckOwnershipBinding) {
        if self.borrowck_ownership_model_enabled() && !binding.span.from_expansion() {
            self.borrowck_repairs.ownership_bindings.lock().push(binding);
        }
    }

    pub fn record_borrowck_ownership_loan(&self, loan: BorrowckOwnershipLoan) {
        if self.borrowck_ownership_model_enabled() && !loan.binding_span.from_expansion() {
            self.borrowck_repairs.ownership_loans.lock().push(loan);
        }
    }

    pub fn record_borrowck_ownership_event(&self, event: BorrowckOwnershipEvent) {
        if !self.borrowck_ownership_model_enabled() {
            return;
        }
        if event.span.from_expansion() || event.binding_span.from_expansion() {
            return;
        }
        self.borrowck_repairs.ownership_events.lock().push(event);
    }

    pub fn record_borrowck_repair(&self, kind: BorrowckRepairKind, edits: Vec<(Span, String)>) {
        self.record_borrowck_repair_for_binding(kind, None, edits);
    }

    pub fn record_borrowck_repair_for_binding(
        &self,
        kind: BorrowckRepairKind,
        binding_span: Option<Span>,
        edits: Vec<(Span, String)>,
    ) {
        if self.opts.unstable_opts.borrowck_autofix.is_none()
            && !self.opts.unstable_opts.borrowck_wrapper_suggestions
        {
            return;
        }
        if std::env::var_os(AUTOFIX_PLAN_ENV).is_none() {
            return;
        }
        if edits.is_empty() || edits.iter().any(|(span, _)| span.from_expansion()) {
            return;
        }
        self.borrowck_repairs.repairs.lock().push(BorrowckRepairGroup {
            kind,
            binding_span,
            edits: edits
                .into_iter()
                .map(|(span, replacement)| BorrowckRepairEdit { span, replacement })
                .collect(),
        });
    }

    pub fn record_borrowck_wrapper_intent(&self, intent: BorrowckWrapperIntent) {
        if !(self.opts.unstable_opts.borrowck_autofix.is_some()
            && self.opts.unstable_opts.borrowck_autofix_wrapper_variants)
            && !self.opts.unstable_opts.borrowck_wrapper_suggestions
        {
            return;
        }
        if std::env::var_os(AUTOFIX_PLAN_ENV).is_none() {
            return;
        }
        self.borrowck_repairs.wrapper_intents.lock().push(intent);
    }

    pub fn record_borrowck_wrapper_variant(&self, variant: BorrowckWrapperVariant) {
        self.borrowck_repairs.wrapper_variants.lock().push(variant);
    }

    pub fn record_borrowck_wrapper_rejection(&self, rejection: BorrowckWrapperRejection) {
        self.borrowck_repairs.wrapper_rejections.lock().push(rejection);
    }

    pub fn write_borrowck_repair_plan(
        &self,
        crate_name: String,
        stable_crate_id: u64,
    ) -> io::Result<()> {
        let orchestrated_plan_path = std::env::var_os(AUTOFIX_PLAN_ENV).map(PathBuf::from);
        let direct_model_directory = self.opts.unstable_opts.borrowck_ownership_model.clone();
        if orchestrated_plan_path.is_none() && direct_model_directory.is_none() {
            return Ok(());
        }

        let source_map = self.source_map();
        let mut sources = BTreeMap::new();
        let mut repairs = Vec::new();

        let mut serialize_span = |span: Span| -> Option<(PathBuf, usize, usize)> {
            let lo = source_map.lookup_byte_offset(span.lo());
            let hi = source_map.lookup_byte_offset(span.hi());
            if lo.sf.start_pos != hi.sf.start_pos {
                return None;
            }
            let FileName::Real(real_name) = &lo.sf.name else {
                return None;
            };
            let local_path = real_name.local_path()?;
            let path = if local_path.is_absolute() {
                local_path.to_path_buf()
            } else {
                source_map
                    .working_dir()
                    .local_path()
                    .expect("the compiler working directory should be local")
                    .join(local_path)
            };
            let path = path.canonicalize().unwrap_or(path);
            let source = lo.sf.src.as_deref()?;
            sources.entry(path.clone()).or_insert_with(|| SerializedBorrowckRepairSource {
                path: path.clone(),
                source_hash: stable_source_hash(source),
                source: Some(source.to_string()),
            });
            Some((path, lo.pos.to_usize(), hi.pos.to_usize()))
        };
        let serialize_binding =
            |span: Span,
             name: &str,
             serialize_span: &mut dyn FnMut(Span) -> Option<(PathBuf, usize, usize)>| {
                let (path, byte_start, _) = serialize_span(span)?;
                Some(SerializedBorrowckBinding { path, byte_start, name: name.to_string() })
            };
        let serialize_edits =
            |edits: &[BorrowckRepairEdit],
             serialize_span: &mut dyn FnMut(Span) -> Option<(PathBuf, usize, usize)>| {
                edits
                    .iter()
                    .map(|edit| {
                        let (path, byte_start, byte_end) = serialize_span(edit.span)?;
                        Some(SerializedBorrowckRepairEdit {
                            path,
                            byte_start,
                            byte_end,
                            replacement: edit.replacement.clone(),
                        })
                    })
                    .collect::<Option<Vec<_>>>()
            };

        for group in self.borrowck_repairs.repairs.lock().iter() {
            if let Some(serialized_edits) = serialize_edits(&group.edits, &mut serialize_span)
                && !serialized_edits.is_empty()
            {
                repairs.push(SerializedBorrowckRepairGroup {
                    kind: group.kind,
                    binding: group.binding_span.and_then(|span| {
                        let name = source_map.span_to_snippet(span).unwrap_or_default();
                        serialize_binding(span, &name, &mut serialize_span)
                    }),
                    edits: serialized_edits,
                });
            }
        }

        let mut wrapper_variants = Vec::new();
        for variant in self.borrowck_repairs.wrapper_variants.lock().iter() {
            if let Some(binding) =
                serialize_binding(variant.binding_span, &variant.binding_name, &mut serialize_span)
                && let Some((path, byte_start, byte_end)) = serialize_span(variant.trigger_span)
                && let Some(edits) = serialize_edits(&variant.edits, &mut serialize_span)
                && !edits.is_empty()
            {
                wrapper_variants.push(SerializedBorrowckWrapperVariant {
                    binding,
                    trigger: SerializedBorrowckSpan { path, byte_start, byte_end },
                    strategy: variant.strategy,
                    edits,
                });
            }
        }

        let mut wrapper_rejections = Vec::new();
        for rejection in self.borrowck_repairs.wrapper_rejections.lock().iter() {
            if let Some(binding) = serialize_binding(
                rejection.binding_span,
                &rejection.binding_name,
                &mut serialize_span,
            ) {
                wrapper_rejections.push(SerializedBorrowckWrapperRejection {
                    binding,
                    strategy: rejection.strategy,
                    reason: rejection.reason.clone(),
                });
            }
        }

        let mut ownership_bodies = Vec::new();
        for body in self.borrowck_repairs.ownership_bodies.lock().iter() {
            let Some((path, byte_start, byte_end)) = serialize_span(body.span) else { continue };
            let blocks = body
                .blocks
                .iter()
                .filter_map(|block| {
                    let (path, byte_start, byte_end) = serialize_span(block.span)?;
                    Some(SerializedBorrowckOwnershipBlock {
                        basic_block: block.basic_block,
                        span: SerializedBorrowckSpan { path, byte_start, byte_end },
                        successors: block.successors.clone(),
                    })
                })
                .collect();
            ownership_bodies.push(SerializedBorrowckOwnershipBody {
                body_id: body.body_id,
                name: body.name.clone(),
                span: SerializedBorrowckSpan { path, byte_start, byte_end },
                blocks,
            });
        }
        ownership_bodies.sort_by_key(|body| body.body_id);
        ownership_bodies.dedup_by_key(|body| body.body_id);

        let mut ownership_bindings = Vec::new();
        for binding in self.borrowck_repairs.ownership_bindings.lock().iter() {
            let Some(serialized_binding) =
                serialize_binding(binding.span, &binding.name, &mut serialize_span)
            else {
                continue;
            };
            ownership_bindings.push(SerializedBorrowckOwnershipBinding {
                body_id: binding.body_id,
                binding: serialized_binding,
                type_name: binding.type_name.clone(),
                size: binding.size,
                align: binding.align,
                memory_layers: binding
                    .memory_layers
                    .iter()
                    .map(|layer| SerializedBorrowckMemoryLayer {
                        kind: layer.kind,
                        storage: layer.storage,
                        label: layer.label.clone(),
                        type_name: layer.type_name.clone(),
                        size: layer.size,
                        align: layer.align,
                        provenance: if layer.size.is_some()
                            && !matches!(
                                layer.kind,
                                BorrowckMemoryKind::ArcAllocation
                                    | BorrowckMemoryKind::ContainerBuffer
                                    | BorrowckMemoryKind::FatPointerMetadata
                                    | BorrowckMemoryKind::PinConstraint
                                    | BorrowckMemoryKind::RcAllocation
                                    | BorrowckMemoryKind::StringBuffer
                                    | BorrowckMemoryKind::VecBuffer
                                    | BorrowckMemoryKind::WeakAllocation
                            ) {
                            "target_layout"
                        } else {
                            "conceptual"
                        }
                        .to_string(),
                    })
                    .collect(),
            });
        }
        ownership_bindings.sort_by(|left, right| {
            (left.body_id, &left.binding.path, left.binding.byte_start, &left.binding.name).cmp(&(
                right.body_id,
                &right.binding.path,
                right.binding.byte_start,
                &right.binding.name,
            ))
        });
        ownership_bindings.dedup_by(|left, right| {
            left.body_id == right.body_id
                && left.binding.path == right.binding.path
                && left.binding.byte_start == right.binding.byte_start
        });

        let serialize_loan_point =
            |point: &BorrowckOwnershipLoanPoint,
             serialize_span: &mut dyn FnMut(Span) -> Option<(PathBuf, usize, usize)>| {
                let (path, byte_start, byte_end) = serialize_span(point.span)?;
                Some(SerializedBorrowckOwnershipLoanPoint {
                    basic_block: point.basic_block,
                    statement_index: point.statement_index,
                    span: SerializedBorrowckSpan { path, byte_start, byte_end },
                })
            };
        let mut ownership_loans = Vec::new();
        for loan in self.borrowck_repairs.ownership_loans.lock().iter() {
            let Some(binding) =
                serialize_binding(loan.binding_span, &loan.binding_name, &mut serialize_span)
            else {
                continue;
            };
            let Some(reserve) = serialize_loan_point(&loan.reserve, &mut serialize_span) else {
                continue;
            };
            let activation = loan
                .activation
                .as_ref()
                .and_then(|point| serialize_loan_point(point, &mut serialize_span));
            let live_points = loan
                .live_points
                .iter()
                .filter_map(|point| serialize_loan_point(point, &mut serialize_span))
                .collect();
            let end_points = loan
                .end_points
                .iter()
                .filter_map(|point| serialize_loan_point(point, &mut serialize_span))
                .collect();
            ownership_loans.push(SerializedBorrowckOwnershipLoan {
                body_id: loan.body_id,
                loan_id: loan.loan_id,
                kind: loan.kind,
                binding,
                place: loan.place.clone(),
                reserve,
                activation,
                live_points,
                end_points,
                truncated: loan.truncated,
            });
        }
        ownership_loans.sort_by_key(|loan| (loan.body_id, loan.loan_id));
        ownership_loans.dedup_by_key(|loan| (loan.body_id, loan.loan_id));

        let mut ownership_events = Vec::new();
        for event in self.borrowck_repairs.ownership_events.lock().iter() {
            if let Some((path, byte_start, byte_end)) = serialize_span(event.span)
                && let Some(binding) =
                    serialize_binding(event.binding_span, &event.binding_name, &mut serialize_span)
            {
                ownership_events.push(SerializedBorrowckOwnershipEvent {
                    event_id: ownership_event_id(event),
                    body_id: event.body_id,
                    basic_block: event.basic_block,
                    statement_index: event.statement_index,
                    kind: event.kind,
                    state: event.state,
                    path,
                    byte_start,
                    byte_end,
                    binding,
                    place: event.place.clone(),
                    loan_id: event.loan_id,
                    detail: event.detail.clone(),
                    destination: event.destination.as_ref().map(|destination| {
                        SerializedBorrowckOwnershipDestination {
                            kind: destination.kind,
                            label: destination.label.clone(),
                            place: destination.place.clone(),
                            span: destination.span.and_then(|span| {
                                let (path, byte_start, byte_end) = serialize_span(span)?;
                                Some(SerializedBorrowckSpan { path, byte_start, byte_end })
                            }),
                        }
                    }),
                });
            }
        }
        ownership_events.sort_by(|left, right| {
            (
                &left.path,
                left.byte_start,
                left.body_id,
                left.basic_block,
                left.statement_index,
                left.kind,
                &left.place,
                left.loan_id,
            )
                .cmp(&(
                    &right.path,
                    right.byte_start,
                    right.body_id,
                    right.basic_block,
                    right.statement_index,
                    right.kind,
                    &right.place,
                    right.loan_id,
                ))
        });
        ownership_events.dedup_by(|left, right| left.event_id == right.event_id);

        let memory_graph = build_ownership_memory_graph(
            &sources,
            &ownership_bindings,
            &ownership_events,
            &ownership_loans,
        );
        let mut plan = SerializedBorrowckRepairPlan {
            schema_version: BORROWCK_OWNERSHIP_MODEL_SCHEMA_VERSION,
            target_triple: self.opts.target_triple.to_string(),
            crate_name,
            stable_crate_id,
            sources: sources.into_values().collect(),
            ownership_bodies,
            ownership_bindings,
            ownership_events,
            ownership_loans,
            memory_graph,
            repairs,
            wrapper_variants,
            wrapper_rejections,
        };
        if let Some(plan_path) = orchestrated_plan_path {
            if let Some(parent) = plan_path.parent() {
                fs::create_dir_all(parent)?;
            }
            return serde_json::to_writer_pretty(File::create(plan_path)?, &plan)
                .map_err(io::Error::other);
        }

        let directory = direct_model_directory.expect("checked above");
        fs::create_dir_all(&directory)?;
        let content_hash = ownership_plan_content_hash(&plan);
        let model_path = directory
            .join(format!("{}-{:016x}-{content_hash}.json", plan.crate_name, plan.stable_crate_id));
        // rust-analyzer already has the live VFS text. Omitting it from the persistent artifact
        // avoids retaining a second copy for every crate while the stable hash still rejects stale
        // byte offsets.
        for source in &mut plan.sources {
            source.source = None;
        }
        write_atomic_json(&model_path, &plan)
    }
}

fn graph_id(prefix: &str, identity: &str) -> String {
    format!("{prefix}-{}", stable_bytes_hash(identity.as_bytes()))
}

fn binding_node_id(
    binding: &SerializedBorrowckOwnershipBinding,
    bindings: &[SerializedBorrowckOwnershipBinding],
) -> String {
    let mut same_name = bindings
        .iter()
        .filter(|candidate| {
            candidate.body_id == binding.body_id
                && candidate.binding.path == binding.binding.path
                && candidate.binding.name == binding.binding.name
        })
        .collect::<Vec<_>>();
    same_name.sort_by_key(|candidate| candidate.binding.byte_start);
    let shadow_ordinal = same_name
        .iter()
        .position(|candidate| candidate.binding.byte_start == binding.binding.byte_start)
        .unwrap_or(0);
    graph_id(
        "node",
        &format!(
            "{}:{}:{}:{}:{shadow_ordinal}:binding",
            binding.body_id,
            binding.binding.path.display(),
            binding.binding.name,
            binding.type_name,
        ),
    )
}

fn layer_node_kind(kind: BorrowckMemoryKind) -> BorrowckGraphNodeKind {
    match kind {
        BorrowckMemoryKind::StackBinding => BorrowckGraphNodeKind::Binding,
        BorrowckMemoryKind::ArcHandle
        | BorrowckMemoryKind::BoxHandle
        | BorrowckMemoryKind::RawPointer
        | BorrowckMemoryKind::RcHandle
        | BorrowckMemoryKind::ReferenceHandle
        | BorrowckMemoryKind::WeakHandle => BorrowckGraphNodeKind::Handle,
        BorrowckMemoryKind::ContainerHeader
        | BorrowckMemoryKind::PinConstraint
        | BorrowckMemoryKind::StringHeader
        | BorrowckMemoryKind::VecHeader => BorrowckGraphNodeKind::Wrapper,
        BorrowckMemoryKind::AggregateValue
        | BorrowckMemoryKind::CellState
        | BorrowckMemoryKind::ConditionalValue
        | BorrowckMemoryKind::InlineValue
        | BorrowckMemoryKind::UnsafeCellState => BorrowckGraphNodeKind::InlineValue,
        BorrowckMemoryKind::BoxAllocation => BorrowckGraphNodeKind::HeapAllocation,
        BorrowckMemoryKind::RcAllocation
        | BorrowckMemoryKind::ArcAllocation
        | BorrowckMemoryKind::WeakAllocation => BorrowckGraphNodeKind::ControlBlock,
        BorrowckMemoryKind::ContainerBuffer
        | BorrowckMemoryKind::StringBuffer
        | BorrowckMemoryKind::VecBuffer => BorrowckGraphNodeKind::Buffer,
        BorrowckMemoryKind::RefCellState => BorrowckGraphNodeKind::BorrowFlag,
        BorrowckMemoryKind::GuardState => BorrowckGraphNodeKind::Guard,
        BorrowckMemoryKind::MutexState
        | BorrowckMemoryKind::OnceState
        | BorrowckMemoryKind::RwLockState => BorrowckGraphNodeKind::LockState,
        BorrowckMemoryKind::FatPointerMetadata => BorrowckGraphNodeKind::Metadata,
    }
}

fn graph_storage(storage: BorrowckMemoryStorage) -> BorrowckGraphStorageRegion {
    match storage {
        BorrowckMemoryStorage::Conceptual => BorrowckGraphStorageRegion::Conceptual,
        BorrowckMemoryStorage::Heap => BorrowckGraphStorageRegion::Heap,
        BorrowckMemoryStorage::Inline => BorrowckGraphStorageRegion::Inline,
        BorrowckMemoryStorage::Stack => BorrowckGraphStorageRegion::Stack,
    }
}

fn graph_provenance(layer: &SerializedBorrowckMemoryLayer) -> BorrowckOwnershipProvenance {
    match layer.provenance.as_str() {
        "target_layout" => BorrowckOwnershipProvenance::Exact,
        "derived" => BorrowckOwnershipProvenance::Derived,
        "conceptual" => BorrowckOwnershipProvenance::Conceptual,
        _ => BorrowckOwnershipProvenance::Unknown,
    }
}

fn topology_relation(
    source: BorrowckMemoryKind,
    target: BorrowckMemoryKind,
) -> BorrowckGraphEdgeRelation {
    if source == BorrowckMemoryKind::StackBinding {
        return BorrowckGraphEdgeRelation::Stores;
    }
    if source == BorrowckMemoryKind::PinConstraint {
        return BorrowckGraphEdgeRelation::Wraps;
    }
    if matches!(
        source,
        BorrowckMemoryKind::CellState
            | BorrowckMemoryKind::GuardState
            | BorrowckMemoryKind::MutexState
            | BorrowckMemoryKind::OnceState
            | BorrowckMemoryKind::RefCellState
            | BorrowckMemoryKind::RwLockState
            | BorrowckMemoryKind::UnsafeCellState
    ) {
        return BorrowckGraphEdgeRelation::GuardsAccess;
    }
    match target {
        BorrowckMemoryKind::BoxAllocation => BorrowckGraphEdgeRelation::Owns,
        BorrowckMemoryKind::RcAllocation | BorrowckMemoryKind::ArcAllocation => {
            BorrowckGraphEdgeRelation::SharesAllocation
        }
        BorrowckMemoryKind::WeakAllocation => BorrowckGraphEdgeRelation::WeakReference,
        BorrowckMemoryKind::ContainerBuffer
        | BorrowckMemoryKind::StringBuffer
        | BorrowckMemoryKind::VecBuffer => BorrowckGraphEdgeRelation::OwnsBuffer,
        BorrowckMemoryKind::ConditionalValue => BorrowckGraphEdgeRelation::Conditional,
        BorrowckMemoryKind::GuardState => BorrowckGraphEdgeRelation::GuardsAccess,
        BorrowckMemoryKind::PinConstraint => BorrowckGraphEdgeRelation::Wraps,
        BorrowckMemoryKind::StackBinding
        | BorrowckMemoryKind::AggregateValue
        | BorrowckMemoryKind::ArcHandle
        | BorrowckMemoryKind::BoxHandle
        | BorrowckMemoryKind::CellState
        | BorrowckMemoryKind::ContainerHeader
        | BorrowckMemoryKind::FatPointerMetadata
        | BorrowckMemoryKind::InlineValue
        | BorrowckMemoryKind::MutexState
        | BorrowckMemoryKind::OnceState
        | BorrowckMemoryKind::RawPointer
        | BorrowckMemoryKind::RcHandle
        | BorrowckMemoryKind::ReferenceHandle
        | BorrowckMemoryKind::RefCellState
        | BorrowckMemoryKind::RwLockState
        | BorrowckMemoryKind::StringHeader
        | BorrowckMemoryKind::UnsafeCellState
        | BorrowckMemoryKind::VecHeader
        | BorrowckMemoryKind::WeakHandle => BorrowckGraphEdgeRelation::Contains,
    }
}

fn snapshot_kind(kind: BorrowckOwnershipEventKind) -> BorrowckSnapshotKind {
    match kind {
        BorrowckOwnershipEventKind::BorrowActivate => BorrowckSnapshotKind::BorrowActivate,
        BorrowckOwnershipEventKind::BorrowEnd => BorrowckSnapshotKind::BorrowEnd,
        BorrowckOwnershipEventKind::BorrowMutable | BorrowckOwnershipEventKind::BorrowShared => {
            BorrowckSnapshotKind::BorrowReserve
        }
        BorrowckOwnershipEventKind::Clone => BorrowckSnapshotKind::Clone,
        BorrowckOwnershipEventKind::Copy => BorrowckSnapshotKind::Copy,
        BorrowckOwnershipEventKind::Drop => BorrowckSnapshotKind::Drop,
        BorrowckOwnershipEventKind::LastUse => BorrowckSnapshotKind::LastUse,
        BorrowckOwnershipEventKind::Move => BorrowckSnapshotKind::Move,
        BorrowckOwnershipEventKind::PartialMove => BorrowckSnapshotKind::PartialMove,
        BorrowckOwnershipEventKind::Reinitialize => BorrowckSnapshotKind::Reinitialize,
    }
}

fn find_binding_node_id(
    bindings: &[SerializedBorrowckOwnershipBinding],
    body_id: u64,
    place: &str,
) -> Option<String> {
    let root = place.trim_start_matches('*').split(['.', '[', ':']).next().unwrap_or(place);
    bindings
        .iter()
        .find(|binding| binding.body_id == body_id && binding.binding.name == root)
        .map(|binding| binding_node_id(binding, bindings))
}

fn ensure_place_node(
    graph: &mut SerializedBorrowckMemoryGraph,
    bindings: &[SerializedBorrowckOwnershipBinding],
    body_id: u64,
    place: &str,
    span: Option<SerializedBorrowckSpan>,
    provenance: BorrowckOwnershipProvenance,
) -> Option<String> {
    let root_id = find_binding_node_id(bindings, body_id, place)?;
    let root_name = place.trim_start_matches('*').split(['.', '[', ':']).next().unwrap_or(place);
    if place == root_name {
        return Some(root_id);
    }
    let id = graph_id("place", &format!("{body_id}:{place}"));
    if !graph.nodes.iter().any(|node| node.id == id) {
        graph.nodes.push(SerializedBorrowckMemoryNode {
            id: id.clone(),
            body_id,
            place: place.to_string(),
            kind: BorrowckGraphNodeKind::ProjectedPlace,
            storage: BorrowckGraphStorageRegion::Inline,
            label: place.to_string(),
            type_name: "projected place (type available from MIR owner)".to_string(),
            size: None,
            align: None,
            span: span.clone(),
            state: BorrowckOwnershipState::Available,
            provenance,
            physical_placement_note:
                "A source/MIR subplace of its owner; optimized machine placement may differ."
                    .to_string(),
            truncated: false,
        });
        graph.edges.push(SerializedBorrowckMemoryEdge {
            id: graph_id("edge", &format!("{root_id}:contains:{id}")),
            source: root_id,
            target: id.clone(),
            relation: BorrowckGraphEdgeRelation::Contains,
            event_id: None,
            loan_id: None,
            span,
            provenance,
            path_marker: None,
        });
    }
    Some(id)
}

fn access_step(
    kind: BorrowckAccessKind,
    starting_type: &str,
    result_type: &str,
    mutability: BorrowckAccessMutability,
    explicitness: BorrowckAccessExplicitness,
    fallible: bool,
    may_panic: bool,
    requires_unsafe: bool,
    explanation: &str,
) -> SerializedBorrowckAccessStep {
    SerializedBorrowckAccessStep {
        kind,
        starting_type: starting_type.to_string(),
        result_type: result_type.to_string(),
        mutability,
        explicitness,
        fallible,
        may_panic,
        requires_unsafe,
        explanation: explanation.to_string(),
        provenance: BorrowckOwnershipProvenance::Derived,
    }
}

fn access_paths_for_binding(
    binding: &SerializedBorrowckOwnershipBinding,
    node_id: String,
) -> Vec<SerializedBorrowckAccessPath> {
    let ty = binding.type_name.as_str();
    let inner = binding
        .memory_layers
        .iter()
        .skip(1)
        .map(|layer| layer.type_name.as_str())
        .find(|candidate| *candidate != ty)
        .unwrap_or("the inner value");
    let mut paths = Vec::new();
    let mut push = |purpose: &str, steps: Vec<SerializedBorrowckAccessStep>| {
        paths.push(SerializedBorrowckAccessPath {
            id: graph_id("access", &format!("{}:{}:{purpose}", binding.body_id, node_id)),
            body_id: binding.body_id,
            node_id: node_id.clone(),
            place: binding.binding.name.clone(),
            purpose: purpose.to_string(),
            steps,
            provenance: BorrowckOwnershipProvenance::Derived,
        });
    };

    if ty.starts_with("&mut ") {
        push(
            "mutate the referenced value",
            vec![access_step(
                BorrowckAccessKind::BuiltinDeref,
                ty,
                ty.trim_start_matches("&mut "),
                BorrowckAccessMutability::Mutable,
                BorrowckAccessExplicitness::Either,
                false,
                false,
                false,
                "A mutable reference provides exclusive access while its loan is live.",
            )],
        );
    } else if ty.starts_with('&') {
        push(
            "read the referenced value",
            vec![access_step(
                BorrowckAccessKind::BuiltinDeref,
                ty,
                ty.trim_start_matches('&'),
                BorrowckAccessMutability::Shared,
                BorrowckAccessExplicitness::Either,
                false,
                false,
                false,
                "A shared reference permits reads but not mutation through this path.",
            )],
        );
    } else if ty.starts_with("*mut ") || ty.starts_with("*const ") {
        push(
            "dereference the raw pointer",
            vec![access_step(
                BorrowckAccessKind::RawPointerDeref,
                ty,
                ty.split_once(' ').map_or("the pointee", |(_, pointee)| pointee),
                if ty.starts_with("*mut ") {
                    BorrowckAccessMutability::Mutable
                } else {
                    BorrowckAccessMutability::Shared
                },
                BorrowckAccessExplicitness::Explicit,
                false,
                false,
                true,
                "The compiler cannot prove raw-pointer validity; dereferencing requires unsafe.",
            )],
        );
    } else if ty.contains("NonNull<") {
        push(
            "dereference the non-null raw pointer",
            vec![access_step(
                BorrowckAccessKind::RawPointerDeref,
                ty,
                inner,
                BorrowckAccessMutability::Unknown,
                BorrowckAccessExplicitness::Explicit,
                false,
                false,
                true,
                "NonNull proves non-nullness, not validity, alignment, initialization, or alias safety.",
            )],
        );
    } else if ty.contains("RefMut<")
        || ty.contains("MutexGuard<")
        || ty.contains("RwLockWriteGuard<")
    {
        push(
            "mutate through the live guard",
            vec![access_step(
                BorrowckAccessKind::GuardDeref,
                ty,
                inner,
                BorrowckAccessMutability::Mutable,
                BorrowckAccessExplicitness::Either,
                false,
                false,
                false,
                "Dropping the guard releases the runtime borrow or synchronization gate.",
            )],
        );
    } else if ty.contains("Ref<") || ty.contains("RwLockReadGuard<") {
        push(
            "read through the live guard",
            vec![access_step(
                BorrowckAccessKind::GuardDeref,
                ty,
                inner,
                BorrowckAccessMutability::Shared,
                BorrowckAccessExplicitness::Either,
                false,
                false,
                false,
                "Dropping the guard releases the runtime borrow or synchronization gate.",
            )],
        );
    } else if (ty.contains("Rc<") || ty.contains("Arc<")) && ty.contains("RefCell<") {
        push(
            "mutate a shared value through runtime borrow checking",
            vec![
                access_step(
                    BorrowckAccessKind::TraitDeref,
                    ty,
                    "RefCell<inner>",
                    BorrowckAccessMutability::Shared,
                    BorrowckAccessExplicitness::Either,
                    false,
                    false,
                    false,
                    "The shared handle dereferences to the allocation without granting mutable access.",
                ),
                access_step(
                    BorrowckAccessKind::WrapperBorrowMut,
                    "RefCell<inner>",
                    inner,
                    BorrowckAccessMutability::Mutable,
                    BorrowckAccessExplicitness::Explicit,
                    false,
                    true,
                    false,
                    "borrow_mut() checks exclusivity at runtime and can panic on a conflict.",
                ),
            ],
        );
    } else if (ty.contains("Arc<") || ty.contains("Rc<"))
        && (ty.contains("Mutex<") || ty.contains("RwLock<"))
    {
        push(
            "reach a shared value through a synchronization guard",
            vec![
                access_step(
                    BorrowckAccessKind::TraitDeref,
                    ty,
                    "synchronization wrapper",
                    BorrowckAccessMutability::Shared,
                    BorrowckAccessExplicitness::Either,
                    false,
                    false,
                    false,
                    "The shared handle dereferences to the synchronization wrapper.",
                ),
                access_step(
                    BorrowckAccessKind::LockAcquire,
                    "synchronization wrapper",
                    "lock guard",
                    BorrowckAccessMutability::Mutable,
                    BorrowckAccessExplicitness::Explicit,
                    true,
                    false,
                    false,
                    "Acquiring a lock can block and may report poisoning.",
                ),
                access_step(
                    BorrowckAccessKind::GuardDeref,
                    "lock guard",
                    inner,
                    BorrowckAccessMutability::Mutable,
                    BorrowckAccessExplicitness::Either,
                    false,
                    false,
                    false,
                    "The guard grants access until it is dropped.",
                ),
            ],
        );
    } else if ty.contains("RefCell<") && !ty.contains("Weak<") {
        push(
            "read through runtime borrow checking",
            vec![access_step(
                BorrowckAccessKind::WrapperBorrow,
                ty,
                inner,
                BorrowckAccessMutability::Shared,
                BorrowckAccessExplicitness::Explicit,
                false,
                true,
                false,
                "borrow() creates a shared guard and panics if a mutable guard is active.",
            )],
        );
        push(
            "mutate through runtime borrow checking",
            vec![access_step(
                BorrowckAccessKind::WrapperBorrowMut,
                ty,
                inner,
                BorrowckAccessMutability::Mutable,
                BorrowckAccessExplicitness::Explicit,
                false,
                true,
                false,
                "borrow_mut() creates an exclusive guard and panics if any guard is active.",
            )],
        );
    } else if (ty.contains("Mutex<") || ty.contains("RwLock<")) && !ty.contains("Weak<") {
        let read_only = ty.contains("RwLock<");
        push(
            if read_only { "read through a lock guard" } else { "mutate through a lock guard" },
            vec![
                access_step(
                    BorrowckAccessKind::LockAcquire,
                    ty,
                    "lock guard",
                    if read_only {
                        BorrowckAccessMutability::Shared
                    } else {
                        BorrowckAccessMutability::Mutable
                    },
                    BorrowckAccessExplicitness::Explicit,
                    true,
                    false,
                    false,
                    "Lock acquisition can block and returns a Result because a lock may be poisoned.",
                ),
                access_step(
                    BorrowckAccessKind::GuardDeref,
                    "lock guard",
                    inner,
                    if read_only {
                        BorrowckAccessMutability::Shared
                    } else {
                        BorrowckAccessMutability::Mutable
                    },
                    BorrowckAccessExplicitness::Either,
                    false,
                    false,
                    false,
                    "The guard controls how long access to the protected value remains active.",
                ),
            ],
        );
    } else if ty.contains("Weak<") {
        push(
            "attempt to reach the shared allocation",
            vec![
                access_step(
                    BorrowckAccessKind::WeakUpgrade,
                    ty,
                    "Option<shared handle>",
                    BorrowckAccessMutability::Shared,
                    BorrowckAccessExplicitness::Explicit,
                    true,
                    false,
                    false,
                    "upgrade() is fallible because all strong owners may already be gone.",
                ),
                access_step(
                    BorrowckAccessKind::OptionExtract,
                    "Option<shared handle>",
                    inner,
                    BorrowckAccessMutability::Shared,
                    BorrowckAccessExplicitness::Explicit,
                    true,
                    false,
                    false,
                    "Handle the None case before accessing the value.",
                ),
            ],
        );
    } else if ty.contains("Pin<") {
        push(
            "access through the pinned pointer",
            vec![access_step(
                BorrowckAccessKind::TraitDeref,
                ty,
                inner,
                BorrowckAccessMutability::Shared,
                BorrowckAccessExplicitness::Either,
                false,
                false,
                false,
                "Pin constrains movement of the pointee; it is not a separate storage region.",
            )],
        );
    } else if ty.contains("Option<") || ty.contains("Result<") {
        push(
            "extract the active value",
            vec![access_step(
                if ty.contains("Option<") {
                    BorrowckAccessKind::OptionExtract
                } else {
                    BorrowckAccessKind::ResultExtract
                },
                ty,
                inner,
                BorrowckAccessMutability::NotApplicable,
                BorrowckAccessExplicitness::Explicit,
                true,
                false,
                false,
                "The active runtime variant must be handled before the contained value is available.",
            )],
        );
    } else if ty.contains("Box<") {
        push(
            "access the owned heap value",
            vec![access_step(
                BorrowckAccessKind::BuiltinDeref,
                ty,
                inner,
                BorrowckAccessMutability::Mutable,
                BorrowckAccessExplicitness::Either,
                false,
                false,
                false,
                "Box uniquely owns its allocation, so a mutable Box can provide mutable access.",
            )],
        );
    } else if ty.contains("Rc<") || ty.contains("Arc<") {
        push(
            "read the shared allocation",
            vec![access_step(
                BorrowckAccessKind::TraitDeref,
                ty,
                inner,
                BorrowckAccessMutability::Shared,
                BorrowckAccessExplicitness::Either,
                false,
                false,
                false,
                "Shared ownership provides shared dereference; mutation needs uniqueness or interior mutability.",
            )],
        );
    }
    paths
}

fn build_ownership_memory_graph(
    sources: &BTreeMap<PathBuf, SerializedBorrowckRepairSource>,
    bindings: &[SerializedBorrowckOwnershipBinding],
    events: &[SerializedBorrowckOwnershipEvent],
    loans: &[SerializedBorrowckOwnershipLoan],
) -> SerializedBorrowckMemoryGraph {
    let mut graph = SerializedBorrowckMemoryGraph::default();
    for binding in bindings {
        let binding_id = binding_node_id(binding, bindings);
        let binding_span = SerializedBorrowckSpan {
            path: binding.binding.path.clone(),
            byte_start: binding.binding.byte_start,
            byte_end: binding.binding.byte_start + binding.binding.name.len(),
        };
        let mut previous: Option<(String, BorrowckMemoryKind)> = None;
        for (index, layer) in binding.memory_layers.iter().enumerate().take(12) {
            let id = if index == 0 {
                binding_id.clone()
            } else {
                graph_id(
                    "node",
                    &format!("{}:{:?}:{}:{}", binding_id, layer.kind, layer.label, index),
                )
            };
            graph.nodes.push(SerializedBorrowckMemoryNode {
                id: id.clone(),
                body_id: binding.body_id,
                place: binding.binding.name.clone(),
                kind: layer_node_kind(layer.kind),
                storage: graph_storage(layer.storage),
                label: layer.label.clone(),
                type_name: layer.type_name.clone(),
                size: layer.size,
                align: layer.align,
                span: Some(binding_span.clone()),
                state: BorrowckOwnershipState::Available,
                provenance: graph_provenance(layer),
                physical_placement_note:
                    "Source-level model; optimized machine placement may differ.".to_string(),
                truncated: binding.memory_layers.len() > 12 && index == 11,
            });
            if let Some((source, source_kind)) = previous {
                let relation = topology_relation(source_kind, layer.kind);
                graph.edges.push(SerializedBorrowckMemoryEdge {
                    id: graph_id("edge", &format!("{source}:{relation:?}:{id}")),
                    source,
                    target: id.clone(),
                    relation,
                    event_id: None,
                    loan_id: None,
                    span: Some(binding_span.clone()),
                    provenance: graph_provenance(layer),
                    path_marker: None,
                });
            }
            previous = Some((id, layer.kind));
        }
        graph.truncated |= binding.memory_layers.len() > 12;
        let initialization_id = graph_id("initialize", &binding_id);
        graph.snapshots.push(SerializedBorrowckOwnershipSnapshot {
            id: graph_id("snapshot", &initialization_id),
            event_id: initialization_id,
            body_id: binding.body_id,
            basic_block: 0,
            statement_index: 0,
            kind: BorrowckSnapshotKind::Initialize,
            span: binding_span,
            place: binding.binding.name.clone(),
            loan_id: None,
            path_marker: Some("binding_initialization".to_string()),
            deltas: vec![SerializedBorrowckStateDelta {
                node_id: binding_id.clone(),
                from: None,
                to: BorrowckOwnershipState::Available,
                relation_added: None,
                relation_removed: None,
            }],
            provenance: BorrowckOwnershipProvenance::Derived,
        });
        graph.access_paths.extend(access_paths_for_binding(binding, binding_id));
    }

    for destination in bindings {
        let Some(source_name) = shared_clone_source(sources, destination) else {
            continue;
        };
        let Some(source) = bindings.iter().find(|source| {
            source.body_id == destination.body_id && source.binding.name == source_name
        }) else {
            continue;
        };
        let source_root = binding_node_id(source, bindings);
        let destination_root = binding_node_id(destination, bindings);
        let Some((_, source_allocation, relation)) = allocation_link_for_root(&graph, &source_root)
        else {
            continue;
        };
        if !matches!(
            relation,
            BorrowckGraphEdgeRelation::SharesAllocation | BorrowckGraphEdgeRelation::WeakReference
        ) {
            continue;
        }
        let Some((destination_handle, destination_allocation, _)) =
            allocation_link_for_root(&graph, &destination_root)
        else {
            continue;
        };
        remove_duplicate_allocation(&mut graph, &destination_allocation);
        let span = SerializedBorrowckSpan {
            path: destination.binding.path.clone(),
            byte_start: destination.binding.byte_start,
            byte_end: destination.binding.byte_start + destination.binding.name.len(),
        };
        let event_id = graph_id(
            "derived-clone",
            &format!(
                "{}:{}:{}:{}",
                destination.body_id,
                destination.binding.path.display(),
                source.binding.name,
                destination.binding.name
            ),
        );
        graph.edges.push(SerializedBorrowckMemoryEdge {
            id: graph_id("edge", &format!("{destination_handle}:{relation:?}:{source_allocation}")),
            source: destination_handle,
            target: source_allocation,
            relation,
            event_id: Some(event_id.clone()),
            loan_id: None,
            span: Some(span.clone()),
            provenance: BorrowckOwnershipProvenance::Derived,
            path_marker: Some("source_expression".to_string()),
        });
        graph.snapshots.push(SerializedBorrowckOwnershipSnapshot {
            id: graph_id("snapshot", &event_id),
            event_id,
            body_id: destination.body_id,
            basic_block: 0,
            statement_index: 0,
            kind: BorrowckSnapshotKind::Clone,
            span,
            place: source.binding.name.clone(),
            loan_id: None,
            path_marker: Some("source_expression".to_string()),
            deltas: vec![SerializedBorrowckStateDelta {
                node_id: destination_root,
                from: None,
                to: BorrowckOwnershipState::Available,
                relation_added: Some(relation),
                relation_removed: None,
            }],
            provenance: BorrowckOwnershipProvenance::Derived,
        });
    }

    for loan in loans {
        let Some(root_id) = ensure_place_node(
            &mut graph,
            bindings,
            loan.body_id,
            &loan.place,
            Some(loan.reserve.span.clone()),
            BorrowckOwnershipProvenance::Exact,
        ) else {
            continue;
        };
        let view_id =
            graph_id("node", &format!("{}:{}:loan:{}", loan.body_id, loan.place, loan.loan_id));
        graph.nodes.push(SerializedBorrowckMemoryNode {
            id: view_id.clone(),
            body_id: loan.body_id,
            place: loan.place.clone(),
            kind: BorrowckGraphNodeKind::BorrowedView,
            storage: BorrowckGraphStorageRegion::Conceptual,
            label: match loan.kind {
                BorrowckLoanKind::Mutable => "exclusive borrowed view",
                BorrowckLoanKind::Shared => "shared borrowed view",
            }
            .to_string(),
            type_name: "borrowed view".to_string(),
            size: None,
            align: None,
            span: Some(loan.reserve.span.clone()),
            state: match loan.kind {
                BorrowckLoanKind::Mutable => BorrowckOwnershipState::MutablyBorrowed,
                BorrowckLoanKind::Shared => BorrowckOwnershipState::SharedBorrowed,
            },
            provenance: BorrowckOwnershipProvenance::Exact,
            physical_placement_note:
                "A MIR loan, not a claim that a separate runtime allocation exists.".to_string(),
            truncated: loan.truncated,
        });
        let relation = match loan.kind {
            BorrowckLoanKind::Mutable => BorrowckGraphEdgeRelation::BorrowMutable,
            BorrowckLoanKind::Shared => BorrowckGraphEdgeRelation::BorrowShared,
        };
        graph.edges.push(SerializedBorrowckMemoryEdge {
            id: graph_id("edge", &format!("{root_id}:{relation:?}:{view_id}:{}", loan.loan_id)),
            source: view_id,
            target: root_id,
            relation,
            event_id: None,
            loan_id: Some(loan.loan_id),
            span: Some(loan.reserve.span.clone()),
            provenance: BorrowckOwnershipProvenance::Exact,
            path_marker: Some(format!("bb{}", loan.reserve.basic_block)),
        });
    }

    for event in events {
        let mut additional_deltas = Vec::new();
        let event_span = SerializedBorrowckSpan {
            path: event.path.clone(),
            byte_start: event.byte_start,
            byte_end: event.byte_end,
        };
        let Some(node_id) = ensure_place_node(
            &mut graph,
            bindings,
            event.body_id,
            &event.place,
            Some(event_span.clone()),
            BorrowckOwnershipProvenance::Exact,
        ) else {
            continue;
        };
        let relation = match event.kind {
            BorrowckOwnershipEventKind::Move | BorrowckOwnershipEventKind::PartialMove => {
                Some(BorrowckGraphEdgeRelation::MovedTo)
            }
            BorrowckOwnershipEventKind::BorrowMutable => {
                Some(BorrowckGraphEdgeRelation::BorrowMutable)
            }
            BorrowckOwnershipEventKind::BorrowShared => {
                Some(BorrowckGraphEdgeRelation::BorrowShared)
            }
            BorrowckOwnershipEventKind::Clone => Some(BorrowckGraphEdgeRelation::SharesAllocation),
            _ => None,
        };
        if matches!(
            event.kind,
            BorrowckOwnershipEventKind::Move | BorrowckOwnershipEventKind::PartialMove
        ) && let Some(destination) = event.destination.as_ref()
            && let Some(destination_place) = destination.place.as_deref()
            && let Some(target) = ensure_place_node(
                &mut graph,
                bindings,
                event.body_id,
                destination_place,
                destination.span.clone(),
                BorrowckOwnershipProvenance::Exact,
            )
        {
            graph.edges.push(SerializedBorrowckMemoryEdge {
                id: graph_id("edge", &format!("{}:moved_to:{target}", event.event_id)),
                source: node_id.clone(),
                target: target.clone(),
                relation: BorrowckGraphEdgeRelation::MovedTo,
                event_id: Some(event.event_id.clone()),
                loan_id: event.loan_id,
                span: Some(event_span.clone()),
                provenance: BorrowckOwnershipProvenance::Exact,
                path_marker: Some(format!("bb{}", event.basic_block)),
            });
            if event.kind == BorrowckOwnershipEventKind::Move
                && let Some((source_handle, source_allocation, allocation_relation)) =
                    allocation_link_for_root(&graph, &node_id)
                && let Some((destination_handle, duplicate_allocation, _)) =
                    allocation_link_for_root(&graph, &target)
            {
                remove_duplicate_allocation(&mut graph, &duplicate_allocation);
                graph.edges.push(SerializedBorrowckMemoryEdge {
                    id: graph_id(
                        "edge",
                        &format!("{}:{allocation_relation:?}:{source_allocation}", event.event_id),
                    ),
                    source: destination_handle,
                    target: source_allocation,
                    relation: allocation_relation,
                    event_id: Some(event.event_id.clone()),
                    loan_id: event.loan_id,
                    span: Some(event_span.clone()),
                    provenance: BorrowckOwnershipProvenance::Exact,
                    path_marker: Some(format!("bb{}", event.basic_block)),
                });
                additional_deltas.push(SerializedBorrowckStateDelta {
                    node_id: source_handle,
                    from: None,
                    to: BorrowckOwnershipState::Moved,
                    relation_added: None,
                    relation_removed: Some(allocation_relation),
                });
            }
        }
        if event.kind == BorrowckOwnershipEventKind::Clone
            && let Some(destination) = event.destination.as_ref()
            && let Some(destination_place) = destination.place.as_deref()
            && let Some(destination_root) = ensure_place_node(
                &mut graph,
                bindings,
                event.body_id,
                destination_place,
                destination.span.clone(),
                BorrowckOwnershipProvenance::Exact,
            )
            && let Some((_, source_allocation, allocation_relation)) =
                allocation_link_for_root(&graph, &node_id)
            && matches!(
                allocation_relation,
                BorrowckGraphEdgeRelation::SharesAllocation
                    | BorrowckGraphEdgeRelation::WeakReference
            )
            && let Some((destination_handle, duplicate_allocation, _)) =
                allocation_link_for_root(&graph, &destination_root)
        {
            remove_duplicate_allocation(&mut graph, &duplicate_allocation);
            graph.edges.push(SerializedBorrowckMemoryEdge {
                id: graph_id(
                    "edge",
                    &format!("{}:{allocation_relation:?}:{source_allocation}", event.event_id),
                ),
                source: destination_handle,
                target: source_allocation,
                relation: allocation_relation,
                event_id: Some(event.event_id.clone()),
                loan_id: event.loan_id,
                span: Some(event_span.clone()),
                provenance: BorrowckOwnershipProvenance::Exact,
                path_marker: Some(format!("bb{}", event.basic_block)),
            });
        }
        let mut deltas = vec![SerializedBorrowckStateDelta {
            node_id,
            from: None,
            to: event.state,
            relation_added: relation,
            relation_removed: if event.kind == BorrowckOwnershipEventKind::BorrowEnd {
                Some(BorrowckGraphEdgeRelation::BorrowShared)
            } else {
                None
            },
        }];
        deltas.extend(additional_deltas);
        graph.snapshots.push(SerializedBorrowckOwnershipSnapshot {
            id: graph_id("snapshot", &event.event_id),
            event_id: event.event_id.clone(),
            body_id: event.body_id,
            basic_block: event.basic_block,
            statement_index: event.statement_index,
            kind: snapshot_kind(event.kind),
            span: event_span,
            place: event.place.clone(),
            loan_id: event.loan_id,
            path_marker: Some(format!("bb{}", event.basic_block)),
            deltas,
            provenance: BorrowckOwnershipProvenance::Exact,
        });
    }

    graph.nodes.sort_by(|left, right| left.id.cmp(&right.id));
    graph.nodes.dedup_by(|left, right| left.id == right.id);
    graph.edges.sort_by(|left, right| left.id.cmp(&right.id));
    graph.edges.dedup_by(|left, right| left.id == right.id);
    graph.snapshots.sort_by_key(|snapshot| {
        (snapshot.body_id, snapshot.basic_block, snapshot.statement_index, snapshot.id.clone())
    });
    graph.access_paths.sort_by(|left, right| left.id.cmp(&right.id));
    const MAX_GRAPH_NODES: usize = 512;
    const MAX_GRAPH_EDGES: usize = 1024;
    const MAX_GRAPH_SNAPSHOTS: usize = 1024;
    const MAX_GRAPH_ACCESS_PATHS: usize = 256;
    graph.truncated |= graph.nodes.len() > MAX_GRAPH_NODES
        || graph.edges.len() > MAX_GRAPH_EDGES
        || graph.snapshots.len() > MAX_GRAPH_SNAPSHOTS
        || graph.access_paths.len() > MAX_GRAPH_ACCESS_PATHS;
    graph.nodes.truncate(MAX_GRAPH_NODES);
    let retained_nodes = graph.nodes.iter().map(|node| node.id.as_str()).collect::<BTreeSet<_>>();
    graph.edges.retain(|edge| {
        retained_nodes.contains(&edge.source.as_str())
            && retained_nodes.contains(&edge.target.as_str())
    });
    graph.edges.truncate(MAX_GRAPH_EDGES);
    graph.snapshots.truncate(MAX_GRAPH_SNAPSHOTS);
    graph.access_paths.truncate(MAX_GRAPH_ACCESS_PATHS);
    graph
}

fn remove_duplicate_allocation(graph: &mut SerializedBorrowckMemoryGraph, allocation: &str) {
    let mut duplicate_nodes = vec![allocation.to_string()];
    let mut cursor = 0;
    while cursor < duplicate_nodes.len() {
        let current = duplicate_nodes[cursor].clone();
        cursor += 1;
        let children = graph
            .edges
            .iter()
            .filter(|edge| edge.source == current)
            .map(|edge| edge.target.clone())
            .collect::<Vec<_>>();
        for child in children {
            if !duplicate_nodes.contains(&child) {
                duplicate_nodes.push(child);
            }
        }
    }
    graph.nodes.retain(|node| !duplicate_nodes.contains(&node.id));
    graph.edges.retain(|edge| {
        !duplicate_nodes.contains(&edge.source) && !duplicate_nodes.contains(&edge.target)
    });
}

fn allocation_link_for_root(
    graph: &SerializedBorrowckMemoryGraph,
    root: &str,
) -> Option<(String, String, BorrowckGraphEdgeRelation)> {
    let mut pending = vec![root.to_string()];
    let mut visited = BTreeSet::new();
    while let Some(source) = pending.pop() {
        if !visited.insert(source.clone()) {
            continue;
        }
        for edge in graph.edges.iter().filter(|edge| edge.source == source) {
            match edge.relation {
                BorrowckGraphEdgeRelation::Owns
                | BorrowckGraphEdgeRelation::OwnsBuffer
                | BorrowckGraphEdgeRelation::SharesAllocation
                | BorrowckGraphEdgeRelation::WeakReference => {
                    return Some((edge.source.clone(), edge.target.clone(), edge.relation));
                }
                BorrowckGraphEdgeRelation::Contains
                | BorrowckGraphEdgeRelation::GuardsAccess
                | BorrowckGraphEdgeRelation::Stores
                | BorrowckGraphEdgeRelation::Wraps => pending.push(edge.target.clone()),
                _ => {}
            }
        }
    }
    None
}

fn shared_clone_source(
    sources: &BTreeMap<PathBuf, SerializedBorrowckRepairSource>,
    destination: &SerializedBorrowckOwnershipBinding,
) -> Option<String> {
    let source = sources.get(&destination.binding.path)?.source.as_deref()?;
    if destination.binding.byte_start > source.len() {
        return None;
    }
    let line_start = source[..destination.binding.byte_start].rfind('\n').map_or(0, |at| at + 1);
    let line_end = source[destination.binding.byte_start..]
        .find('\n')
        .map_or(source.len(), |at| destination.binding.byte_start + at);
    let line = &source[line_start..line_end];
    for marker in ["Rc::clone(&", "Arc::clone(&"] {
        if let Some(start) = line.find(marker) {
            let candidate = &line[start + marker.len()..];
            let end = candidate
                .find(|character: char| !(character == '_' || character.is_alphanumeric()))
                .unwrap_or(candidate.len());
            return (end > 0).then(|| candidate[..end].to_string());
        }
    }
    let assignment = line.split_once('=')?.1;
    let clone_at = assignment.find(".clone()")?;
    let before = assignment[..clone_at].trim_end();
    let start = before
        .rfind(|character: char| !(character == '_' || character.is_alphanumeric()))
        .map_or(0, |at| at + 1);
    (start < before.len()).then(|| before[start..].to_string())
}

fn stable_source_hash(source: &str) -> String {
    stable_bytes_hash(source.as_bytes())
}

fn stable_bytes_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn ownership_plan_content_hash(plan: &SerializedBorrowckRepairPlan) -> String {
    let mut bytes = Vec::new();
    for source in &plan.sources {
        bytes.extend_from_slice(source.path.as_os_str().as_encoded_bytes());
        bytes.extend_from_slice(source.source_hash.as_bytes());
    }
    stable_bytes_hash(&bytes)
}

fn write_atomic_json(path: &std::path::Path, value: &impl Serialize) -> io::Result<()> {
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let result = (|| {
        serde_json::to_writer(File::create(&temporary)?, value).map_err(io::Error::other)?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn ownership_event_id(event: &BorrowckOwnershipEvent) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in event
        .body_id
        .to_le_bytes()
        .into_iter()
        .chain(event.basic_block.to_le_bytes())
        .chain(event.statement_index.to_le_bytes())
        .chain((event.kind as u8).to_le_bytes())
        .chain(event.place.bytes())
        .chain(event.loan_id.unwrap_or(u32::MAX).to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}
