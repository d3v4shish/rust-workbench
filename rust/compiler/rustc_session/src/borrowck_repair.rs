//! Structured source edits produced by conservative borrow-checker diagnostics.

use std::collections::BTreeMap;
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
pub const BORROWCK_OWNERSHIP_MODEL_SCHEMA_VERSION: u32 = 5;

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
    ArcAllocation,
    BoxAllocation,
    InlineValue,
    MutexState,
    RcAllocation,
    RefCellState,
    RwLockState,
    StackBinding,
    StringBuffer,
    StringHeader,
    VecBuffer,
    VecHeader,
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
    pub crate_name: String,
    pub stable_crate_id: u64,
    pub sources: Vec<SerializedBorrowckRepairSource>,
    pub ownership_bodies: Vec<SerializedBorrowckOwnershipBody>,
    pub ownership_bindings: Vec<SerializedBorrowckOwnershipBinding>,
    pub ownership_events: Vec<SerializedBorrowckOwnershipEvent>,
    pub ownership_loans: Vec<SerializedBorrowckOwnershipLoan>,
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
                        provenance: if layer.kind == BorrowckMemoryKind::StackBinding {
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

        let mut plan = SerializedBorrowckRepairPlan {
            schema_version: BORROWCK_OWNERSHIP_MODEL_SCHEMA_VERSION,
            crate_name,
            stable_crate_id,
            sources: sources.into_values().collect(),
            ownership_bodies,
            ownership_bindings,
            ownership_events,
            ownership_loans,
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
