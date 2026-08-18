//! MIR-derived ownership events consumed by editor integrations.

use rustc_data_structures::fx::{FxHashSet, FxIndexMap};
use rustc_hir::def_id::LocalDefId;
use rustc_index::IndexVec;
use rustc_middle::mir::visit::{PlaceContext, Visitor};
use rustc_middle::mir::{
    Body, BorrowKind, Local, Location, Operand, Place, ProjectionElem, RETURN_PLACE, Rvalue,
    Terminator, TerminatorKind, VarDebugInfoContents,
};
use rustc_middle::ty::print::with_no_trimmed_paths;
use rustc_middle::ty::{self, Ty, TyCtxt};
use rustc_mir_dataflow::move_paths::{InitLocation, MoveData};
use rustc_session::borrowck_repair::{
    BorrowckLoanKind, BorrowckMemoryKind, BorrowckMemoryLayer, BorrowckMemoryStorage,
    BorrowckOwnershipBinding, BorrowckOwnershipBlock, BorrowckOwnershipBody,
    BorrowckOwnershipDestination, BorrowckOwnershipDestinationKind, BorrowckOwnershipEvent,
    BorrowckOwnershipEventKind, BorrowckOwnershipLoan, BorrowckOwnershipLoanPoint,
    BorrowckOwnershipState,
};
use rustc_span::Span;

use crate::borrow_set::{BorrowSet, TwoPhaseActivation};
use crate::region_infer::RegionInferenceContext;

const MAX_EVENTS_PER_BODY: usize = 4096;
const MAX_LOAN_POINTS_PER_BODY: usize = 4096;

#[derive(Clone)]
struct Binding {
    name: String,
    span: Span,
}

struct UseCollector<'a> {
    bindings: &'a IndexVec<Local, Option<Binding>>,
    uses: FxIndexMap<Local, Vec<Location>>,
}

struct Transfer<'tcx> {
    kind: BorrowckOwnershipEventKind,
    source: Place<'tcx>,
    location: Location,
    destination: Option<BorrowckOwnershipDestination>,
    destination_local: Option<Local>,
}

struct TransferCollector<'a, 'tcx> {
    bindings: &'a IndexVec<Local, Option<Binding>>,
    transfers: Vec<Transfer<'tcx>>,
    destination: Option<BorrowckOwnershipDestination>,
    destination_local: Option<Local>,
}

impl<'tcx> Visitor<'tcx> for TransferCollector<'_, 'tcx> {
    fn visit_assign(
        &mut self,
        destination: &Place<'tcx>,
        rvalue: &Rvalue<'tcx>,
        location: Location,
    ) {
        let previous = self.destination.take();
        let previous_local = self.destination_local.take();
        self.destination = ownership_destination(self.bindings, *destination);
        self.destination_local = Some(destination.local);
        self.visit_rvalue(rvalue, location);
        self.destination = previous;
        self.destination_local = previous_local;
    }

    fn visit_terminator(&mut self, terminator: &Terminator<'tcx>, location: Location) {
        match &terminator.kind {
            TerminatorKind::Call { func, args, .. }
            | TerminatorKind::TailCall { func, args, .. } => {
                self.visit_operand(func, location);
                for (index, arg) in args.iter().enumerate() {
                    let previous = self.destination.take();
                    let previous_local = self.destination_local.take();
                    let call_destination = BorrowckOwnershipDestination {
                        kind: BorrowckOwnershipDestinationKind::FunctionArgument,
                        label: format!("argument {} of this call", index + 1),
                        place: None,
                        span: Some(terminator.source_info.span),
                    };
                    if let Operand::Move(place) | Operand::Copy(place) = &arg.node
                        && let Some(transfer) = self
                            .transfers
                            .iter_mut()
                            .rev()
                            .find(|transfer| transfer.destination_local == Some(place.local))
                    {
                        transfer.destination = Some(call_destination.clone());
                    }
                    self.destination = Some(call_destination);
                    self.visit_operand(&arg.node, location);
                    self.destination = previous;
                    self.destination_local = previous_local;
                }
            }
            _ => self.super_terminator(terminator, location),
        }
    }

    fn visit_operand(&mut self, operand: &Operand<'tcx>, location: Location) {
        let (kind, source) = match operand {
            Operand::Move(place) => {
                let kind = if place.projection.is_empty() {
                    BorrowckOwnershipEventKind::Move
                } else {
                    BorrowckOwnershipEventKind::PartialMove
                };
                (kind, *place)
            }
            Operand::Copy(place) => (BorrowckOwnershipEventKind::Copy, *place),
            Operand::Constant(_) | Operand::RuntimeChecks(_) => return,
        };
        if self.transfers.len() < MAX_EVENTS_PER_BODY {
            self.transfers.push(Transfer {
                kind,
                source,
                location,
                destination: self.destination.clone(),
                destination_local: self.destination_local,
            });
        }
    }
}

fn ownership_destination(
    bindings: &IndexVec<Local, Option<Binding>>,
    place: Place<'_>,
) -> Option<BorrowckOwnershipDestination> {
    if place.local == RETURN_PLACE {
        return Some(BorrowckOwnershipDestination {
            kind: BorrowckOwnershipDestinationKind::ReturnValue,
            label: "the function return value".to_string(),
            place: Some("return value".to_string()),
            span: None,
        });
    }
    let binding = bindings[place.local].as_ref()?;
    let projected = !place.projection.is_empty();
    Some(BorrowckOwnershipDestination {
        kind: if projected {
            BorrowckOwnershipDestinationKind::ProjectedPlace
        } else {
            BorrowckOwnershipDestinationKind::LocalBinding
        },
        label: place_name(binding, place),
        place: Some(place_name(binding, place)),
        span: Some(binding.span),
    })
}

impl<'tcx> Visitor<'tcx> for UseCollector<'_> {
    fn visit_place(
        &mut self,
        place: &rustc_middle::mir::Place<'tcx>,
        context: PlaceContext,
        location: Location,
    ) {
        if self.bindings[place.local].is_some()
            && context.is_use()
            && !context.is_drop()
            && !context.is_place_assignment()
        {
            let locations = self.uses.entry(place.local).or_default();
            if locations.last() != Some(&location) {
                locations.push(location);
            }
        }
        self.super_place(place, context, location);
    }
}

pub(crate) fn record_ownership_events<'tcx>(
    tcx: TyCtxt<'tcx>,
    def: LocalDefId,
    body: &Body<'tcx>,
    move_data: &MoveData<'tcx>,
    borrow_set: &BorrowSet<'tcx>,
    regioncx: &RegionInferenceContext<'tcx>,
) {
    if !tcx.sess.opts.unstable_opts.borrowck_ownership_events
        && tcx.sess.opts.unstable_opts.borrowck_ownership_model.is_none()
    {
        return;
    }

    // This data is serialized for tooling, not rendered as a diagnostic. Keep all type and def-path
    // formatting out of the diagnostic-only trimmed-path cache; otherwise a successful crate can
    // trip the session assertion that expects a diagnostic after that cache is used.
    with_no_trimmed_paths!({
        let definition_span = tcx.def_span(def);
        // Derived impls and other macro-generated MIR can vastly outnumber the functions the user
        // wrote. Their expansion internals are both noisy in a learning view and expensive to
        // serialize, so keep the exact model anchored to authored source.
        if definition_span.from_expansion() {
            return;
        }
        let body_id = tcx.def_path_hash(def.to_def_id()).local_hash().as_u64();

        tcx.sess.record_borrowck_ownership_body(BorrowckOwnershipBody {
            body_id,
            name: tcx.def_path_str(def.to_def_id()),
            span: definition_span,
            blocks: body
                .basic_blocks
                .iter_enumerated()
                .map(|(block, block_data)| BorrowckOwnershipBlock {
                    basic_block: block.index() as u32,
                    span: body.source_info(Location { block, statement_index: 0 }).span,
                    successors: block_data
                        .terminator()
                        .successors()
                        .map(|successor| successor.index() as u32)
                        .collect(),
                })
                .collect(),
        });

        let mut bindings = IndexVec::from_elem(None, &body.local_decls);
        for debug_info in &body.var_debug_info {
            let VarDebugInfoContents::Place(place) = debug_info.value else { continue };
            if place.projection.is_empty() && bindings[place.local].is_none() {
                bindings[place.local] = Some(Binding {
                    name: debug_info.name.to_string(),
                    span: debug_info.source_info.span,
                });
            }
        }

        for (local, binding) in bindings.iter_enumerated() {
            let Some(binding) = binding else { continue };
            let ty = body.local_decls[local].ty;
            let (size, align) = type_layout(tcx, body, ty);
            let type_name = ty.to_string();
            tcx.sess.record_borrowck_ownership_binding(BorrowckOwnershipBinding {
                body_id,
                span: binding.span,
                name: binding.name.clone(),
                type_name: type_name.clone(),
                size,
                align,
                memory_layers: memory_layers(tcx, body, &binding.name, ty, type_name, size, align),
            });
        }

        let mut events = Vec::new();
        let mut push = |kind,
                        place: Place<'tcx>,
                        location: Location,
                        detail: Option<String>,
                        loan_id: Option<u32>,
                        destination: Option<BorrowckOwnershipDestination>| {
            let Some(binding) = &bindings[place.local] else { return };
            if events.len() < MAX_EVENTS_PER_BODY {
                events.push(BorrowckOwnershipEvent {
                    body_id,
                    basic_block: location.block.index() as u32,
                    statement_index: location.statement_index as u32,
                    kind,
                    state: ownership_state(kind),
                    span: body.source_info(location).span,
                    binding_span: binding.span,
                    binding_name: binding.name.clone(),
                    place: place_name(binding, place),
                    loan_id,
                    detail,
                    destination,
                });
            }
        };

        let mut transfers = TransferCollector {
            bindings: &bindings,
            transfers: Vec::new(),
            destination: None,
            destination_local: None,
        };
        transfers.visit_body(body);
        for transfer in &transfers.transfers {
            push(
                transfer.kind,
                transfer.source,
                transfer.location,
                None,
                None,
                transfer.destination.clone(),
            );
        }

        for (loan_index, borrow) in borrow_set.iter().enumerate() {
            if matches!(borrow.kind, BorrowKind::Fake(_)) {
                continue;
            }
            let kind = match borrow.kind {
                BorrowKind::Shared => BorrowckOwnershipEventKind::BorrowShared,
                BorrowKind::Mut { .. } => BorrowckOwnershipEventKind::BorrowMutable,
                BorrowKind::Fake(_) => unreachable!(),
            };
            let loan_id = Some(loan_index as u32);
            push(kind, borrow.borrowed_place, borrow.reserve_location, None, loan_id, None);

            let activation = match borrow.activation_location {
                TwoPhaseActivation::ActivatedAt(location) => Some(location),
                TwoPhaseActivation::NotActivated | TwoPhaseActivation::NotTwoPhase => None,
            };

            if let Some(location) = activation {
                push(
                    BorrowckOwnershipEventKind::BorrowActivate,
                    borrow.borrowed_place,
                    location,
                    Some("two-phase mutable borrow activates here".to_string()),
                    loan_id,
                    None,
                );
            }

            let mut live_locations = regioncx.region_locations(borrow.region).collect::<Vec<_>>();
            live_locations.sort_unstable_by_key(|location| {
                (location.block.index(), location.statement_index)
            });
            live_locations.dedup();
            let live_location_set = live_locations.iter().copied().collect::<FxHashSet<_>>();
            let mut live_points =
                Vec::with_capacity(live_locations.len().min(MAX_LOAN_POINTS_PER_BODY));
            let mut end_points = Vec::new();
            let truncated = live_locations.len() > MAX_LOAN_POINTS_PER_BODY;
            for &location in &live_locations {
                if live_points.len() < MAX_LOAN_POINTS_PER_BODY {
                    live_points.push(loan_point(body, location));
                }
                let block_data = &body.basic_blocks[location.block];
                let ends_here = if location.statement_index < block_data.statements.len() {
                    !live_location_set.contains(&location.successor_within_block())
                } else {
                    let mut successors = block_data.terminator().successors();
                    match successors.next() {
                        None => true,
                        Some(successor) => {
                            !live_location_set
                                .contains(&Location { block: successor, statement_index: 0 })
                                || successors.any(|successor| {
                                    !live_location_set.contains(&Location {
                                        block: successor,
                                        statement_index: 0,
                                    })
                                })
                        }
                    }
                };
                if ends_here {
                    end_points.push(loan_point(body, location));
                    push(
                        BorrowckOwnershipEventKind::BorrowEnd,
                        borrow.borrowed_place,
                        location,
                        Some("non-lexical borrow ends after this use".to_string()),
                        loan_id,
                        None,
                    );
                }
            }

            if let Some(binding) = &bindings[borrow.borrowed_place.local] {
                tcx.sess.record_borrowck_ownership_loan(BorrowckOwnershipLoan {
                    body_id,
                    loan_id: loan_index as u32,
                    kind: match borrow.kind {
                        BorrowKind::Shared => BorrowckLoanKind::Shared,
                        BorrowKind::Mut { .. } => BorrowckLoanKind::Mutable,
                        BorrowKind::Fake(_) => unreachable!(),
                    },
                    binding_span: binding.span,
                    binding_name: binding.name.clone(),
                    place: place_name(binding, borrow.borrowed_place),
                    reserve: loan_point(body, borrow.reserve_location),
                    activation: activation.map(|location| loan_point(body, location)),
                    live_points,
                    end_points,
                    truncated,
                });
            }
        }

        for init in &move_data.inits {
            let InitLocation::Statement(location) = init.location else { continue };
            let local = move_data.base_local(init.path);
            if transfers.transfers.iter().any(|transfer| {
                transfer.kind != BorrowckOwnershipEventKind::Copy
                    && transfer.source.local == local
                    && transfer.location.is_predecessor_of(location, body)
            }) {
                push(
                    BorrowckOwnershipEventKind::Reinitialize,
                    Place::from(local),
                    location,
                    None,
                    None,
                    None,
                );
            }
        }

        for (block, block_data) in body.basic_blocks.iter_enumerated() {
            if let TerminatorKind::Drop { place, .. } = &block_data.terminator().kind
                && place.projection.is_empty()
            {
                let location = Location { block, statement_index: block_data.statements.len() };
                push(BorrowckOwnershipEventKind::Drop, *place, location, None, None, None);
            }
        }

        let mut uses = UseCollector { bindings: &bindings, uses: FxIndexMap::default() };
        uses.visit_body(body);
        for (local, locations) in uses.uses {
            for &location in &locations {
                if !locations
                    .iter()
                    .any(|&later| later != location && location.is_predecessor_of(later, body))
                {
                    push(
                        BorrowckOwnershipEventKind::LastUse,
                        Place::from(local),
                        location,
                        Some("last use on this control-flow path".to_string()),
                        None,
                        None,
                    );
                }
            }
        }

        for event in events {
            tcx.sess.record_borrowck_ownership_event(event);
        }
    });
}

fn loan_point(body: &Body<'_>, location: Location) -> BorrowckOwnershipLoanPoint {
    BorrowckOwnershipLoanPoint {
        basic_block: location.block.index() as u32,
        statement_index: location.statement_index as u32,
        span: body.source_info(location).span,
    }
}

fn type_layout<'tcx>(
    tcx: TyCtxt<'tcx>,
    body: &Body<'tcx>,
    ty: Ty<'tcx>,
) -> (Option<u64>, Option<u64>) {
    // Borrow checking runs after MIR regions have been renumbered to inference variables. Query
    // keys deliberately reject hashing those variables, while layout does not depend on their
    // identities. Erase them locally before entering the layout query.
    let ty = ty::fold_regions(tcx, ty, |_, _| tcx.lifetimes.re_erased);
    tcx.layout_of(body.typing_env(tcx).as_query_input(ty))
        .ok()
        .map_or((None, None), |layout| (Some(layout.size.bytes()), Some(layout.align.abi.bytes())))
}

fn memory_layers<'tcx>(
    tcx: TyCtxt<'tcx>,
    body: &Body<'tcx>,
    binding_name: &str,
    ty: Ty<'tcx>,
    type_name: String,
    size: Option<u64>,
    align: Option<u64>,
) -> Vec<BorrowckMemoryLayer> {
    let mut layers = vec![BorrowckMemoryLayer {
        kind: BorrowckMemoryKind::StackBinding,
        storage: BorrowckMemoryStorage::Stack,
        label: format!("stack binding `{binding_name}`"),
        type_name,
        size,
        align,
    }];
    push_memory_layers(tcx, body, ty, &mut layers, 0);
    layers
}

fn push_memory_layers<'tcx>(
    tcx: TyCtxt<'tcx>,
    body: &Body<'tcx>,
    ty: Ty<'tcx>,
    layers: &mut Vec<BorrowckMemoryLayer>,
    depth: usize,
) {
    if depth >= 12 {
        return;
    }
    let ty::Adt(def, args) = ty.kind() else {
        let (size, align) = type_layout(tcx, body, ty);
        layers.push(BorrowckMemoryLayer {
            kind: BorrowckMemoryKind::InlineValue,
            storage: BorrowckMemoryStorage::Inline,
            label: "inline value".to_string(),
            type_name: ty.to_string(),
            size,
            align,
        });
        return;
    };
    let path = tcx.def_path_str(def.did());
    let inner = args.types().next();
    let mut push = |kind, storage, label: &str, layer_ty: Ty<'tcx>, exact_layout| {
        let (size, align) =
            if exact_layout { type_layout(tcx, body, layer_ty) } else { (None, None) };
        layers.push(BorrowckMemoryLayer {
            kind,
            storage,
            label: label.to_string(),
            type_name: layer_ty.to_string(),
            size,
            align,
        });
    };

    if path.ends_with("::boxed::Box") {
        if let Some(inner) = inner {
            push(
                BorrowckMemoryKind::BoxAllocation,
                BorrowckMemoryStorage::Heap,
                "owned heap allocation",
                inner,
                true,
            );
            push_memory_layers(tcx, body, inner, layers, depth + 1);
        }
    } else if path.ends_with("::rc::Rc") {
        if let Some(inner) = inner {
            push(
                BorrowckMemoryKind::RcAllocation,
                BorrowckMemoryStorage::Heap,
                "shared allocation with strong and weak counters",
                inner,
                false,
            );
            push_memory_layers(tcx, body, inner, layers, depth + 1);
        }
    } else if path.ends_with("::sync::Arc") {
        if let Some(inner) = inner {
            push(
                BorrowckMemoryKind::ArcAllocation,
                BorrowckMemoryStorage::Heap,
                "thread-safe shared allocation with atomic counters",
                inner,
                false,
            );
            push_memory_layers(tcx, body, inner, layers, depth + 1);
        }
    } else if path.ends_with("::cell::RefCell") {
        if let Some(inner) = inner {
            push(
                BorrowckMemoryKind::RefCellState,
                BorrowckMemoryStorage::Inline,
                "runtime borrow flag and interior value",
                ty,
                true,
            );
            push_memory_layers(tcx, body, inner, layers, depth + 1);
        }
    } else if path.ends_with("::sync::poison::mutex::Mutex") {
        if let Some(inner) = inner {
            push(
                BorrowckMemoryKind::MutexState,
                BorrowckMemoryStorage::Inline,
                "exclusive lock state and interior value",
                ty,
                true,
            );
            push_memory_layers(tcx, body, inner, layers, depth + 1);
        }
    } else if path.ends_with("::sync::poison::rwlock::RwLock") {
        if let Some(inner) = inner {
            push(
                BorrowckMemoryKind::RwLockState,
                BorrowckMemoryStorage::Inline,
                "reader/writer lock state and interior value",
                ty,
                true,
            );
            push_memory_layers(tcx, body, inner, layers, depth + 1);
        }
    } else if path.ends_with("::vec::Vec") {
        push(
            BorrowckMemoryKind::VecHeader,
            BorrowckMemoryStorage::Inline,
            "pointer, length, and capacity",
            ty,
            true,
        );
        if let Some(inner) = inner {
            push(
                BorrowckMemoryKind::VecBuffer,
                BorrowckMemoryStorage::Heap,
                "element buffer (capacity is runtime data)",
                inner,
                false,
            );
        }
    } else if path.ends_with("::string::String") {
        push(
            BorrowckMemoryKind::StringHeader,
            BorrowckMemoryStorage::Inline,
            "UTF-8 pointer, length, and capacity",
            ty,
            true,
        );
        push(
            BorrowckMemoryKind::StringBuffer,
            BorrowckMemoryStorage::Heap,
            "UTF-8 byte buffer (capacity is runtime data)",
            ty,
            false,
        );
    } else {
        let (size, align) = type_layout(tcx, body, ty);
        layers.push(BorrowckMemoryLayer {
            kind: BorrowckMemoryKind::InlineValue,
            storage: BorrowckMemoryStorage::Inline,
            label: "inline value (allocation semantics unknown)".to_string(),
            type_name: ty.to_string(),
            size,
            align,
        });
    }
}

fn ownership_state(kind: BorrowckOwnershipEventKind) -> BorrowckOwnershipState {
    match kind {
        BorrowckOwnershipEventKind::BorrowActivate | BorrowckOwnershipEventKind::BorrowMutable => {
            BorrowckOwnershipState::MutablyBorrowed
        }
        BorrowckOwnershipEventKind::BorrowShared => BorrowckOwnershipState::SharedBorrowed,
        BorrowckOwnershipEventKind::Copy => BorrowckOwnershipState::Available,
        BorrowckOwnershipEventKind::Drop => BorrowckOwnershipState::Dropped,
        BorrowckOwnershipEventKind::Move => BorrowckOwnershipState::Moved,
        BorrowckOwnershipEventKind::PartialMove => BorrowckOwnershipState::PartiallyMoved,
        BorrowckOwnershipEventKind::BorrowEnd
        | BorrowckOwnershipEventKind::LastUse
        | BorrowckOwnershipEventKind::Reinitialize => BorrowckOwnershipState::Available,
    }
}

fn place_name(binding: &Binding, place: Place<'_>) -> String {
    let mut result = binding.name.clone();
    for projection in place.projection {
        match projection {
            ProjectionElem::Deref => result = format!("*{result}"),
            ProjectionElem::Field(field, _) => result.push_str(&format!(".{}", field.index())),
            ProjectionElem::Index(_) => result.push_str("[_]"),
            ProjectionElem::ConstantIndex { offset, from_end, .. } => {
                if from_end {
                    result.push_str(&format!("[-{offset}]"));
                } else {
                    result.push_str(&format!("[{offset}]"));
                }
            }
            ProjectionElem::Subslice { .. } => result.push_str("[..]"),
            ProjectionElem::Downcast(name, variant) => match name {
                Some(name) => result.push_str(&format!("::{name}")),
                None => result.push_str(&format!("::variant{}", variant.index())),
            },
            ProjectionElem::OpaqueCast(_) => result.push_str("::<opaque>"),
            ProjectionElem::UnwrapUnsafeBinder(_) => result.push_str("::<unsafe-binder>"),
        }
    }
    result
}
