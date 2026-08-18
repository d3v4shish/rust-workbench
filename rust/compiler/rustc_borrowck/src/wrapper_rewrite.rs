//! Conservative whole-binding rewrites for opt-in ownership-wrapper variants.

use rustc_hir as hir;
use rustc_hir::def::Res;
use rustc_hir::intravisit::{self, Visitor};
use rustc_middle::ty::TyCtxt;
use rustc_middle::ty::adjustment::{Adjust, AutoBorrow, AutoBorrowMutability, DerefAdjustKind};
use rustc_session::borrowck_repair::{
    BorrowckRepairEdit, BorrowckWrapperIntent, BorrowckWrapperRejection,
    BorrowckWrapperRequirement, BorrowckWrapperSource, BorrowckWrapperStrategy,
    BorrowckWrapperVariant,
};
use rustc_span::sym;

pub fn build_wrapper_variants(tcx: TyCtxt<'_>) {
    if !tcx.sess.opts.unstable_opts.borrowck_autofix_wrapper_variants
        && !tcx.sess.opts.unstable_opts.borrowck_wrapper_suggestions
    {
        return;
    }

    let intents = tcx.sess.borrowck_repairs.wrapper_intents.lock().clone();
    let mut bindings: Vec<Vec<BorrowckWrapperIntent>> = Vec::new();
    for intent in intents {
        if let Some(existing) = bindings.iter_mut().find(|existing| {
            existing[0].body_owner == intent.body_owner
                && existing[0].binding_hir_id == intent.binding_hir_id
        }) {
            if let Some(old) = existing
                .iter_mut()
                .find(|old| old.source == intent.source && old.requirement == intent.requirement)
            {
                // Diagnostics can discover the same repair first at the consuming move and later
                // at the rejected use. Keep the latter as the extra code-action trigger; the
                // rewrite edits themselves already make the action available at the move site.
                old.trigger_span = intent.trigger_span;
            } else {
                existing.push(intent);
            }
        } else {
            bindings.push(vec![intent]);
        }
    }
    bindings.sort_by_key(|intents| intents[0].binding_span.lo());

    for intents in bindings {
        let intent = &intents[0];
        let has_mutation = intents
            .iter()
            .any(|intent| intent.requirement == BorrowckWrapperRequirement::InteriorMutation);
        let primary = match (intent.source, has_mutation) {
            (BorrowckWrapperSource::Box, false) => BorrowckWrapperStrategy::Rc,
            (BorrowckWrapperSource::Box | BorrowckWrapperSource::Rc, true) => {
                BorrowckWrapperStrategy::RcRefCell
            }
            (BorrowckWrapperSource::Plain, true) => BorrowckWrapperStrategy::RefCell,
            (BorrowckWrapperSource::Rc, false) | (BorrowckWrapperSource::Plain, false) => continue,
        };
        let mut strategies = vec![primary];
        if tcx.sess.opts.unstable_opts.borrowck_wrapper_suggestions {
            strategies.extend(match (intent.source, has_mutation) {
                (BorrowckWrapperSource::Box, false) => vec![BorrowckWrapperStrategy::Arc],
                (BorrowckWrapperSource::Box | BorrowckWrapperSource::Rc, true) => {
                    vec![BorrowckWrapperStrategy::ArcMutex, BorrowckWrapperStrategy::ArcRwLock]
                }
                (BorrowckWrapperSource::Plain, true) => {
                    vec![BorrowckWrapperStrategy::Mutex, BorrowckWrapperStrategy::RwLock]
                }
                _ => Vec::new(),
            });
        }

        for strategy in strategies {
            match plan_binding(tcx, intent, strategy) {
                Ok(edits) => tcx.sess.record_borrowck_wrapper_variant(BorrowckWrapperVariant {
                    binding_span: intent.binding_span,
                    trigger_span: intent.trigger_span,
                    binding_name: intent.binding_name.clone(),
                    strategy,
                    edits,
                }),
                Err(reason) => {
                    tcx.sess.record_borrowck_wrapper_rejection(BorrowckWrapperRejection {
                        binding_span: intent.binding_span,
                        binding_name: intent.binding_name.clone(),
                        strategy,
                        reason,
                    })
                }
            }
        }
    }
}

fn plan_binding(
    tcx: TyCtxt<'_>,
    intent: &BorrowckWrapperIntent,
    strategy: BorrowckWrapperStrategy,
) -> Result<Vec<BorrowckRepairEdit>, String> {
    if !is_non_async_function(tcx, intent.body_owner) {
        return Err("binding is not owned by a synchronous function body".into());
    }
    let body = tcx
        .hir_maybe_body_owned_by(intent.body_owner)
        .ok_or_else(|| "binding body is unavailable".to_string())?;
    let local = tcx
        .hir_parent_iter(intent.binding_hir_id)
        .find_map(|(_, node)| match node {
            hir::Node::LetStmt(local) => Some(local),
            _ => None,
        })
        .ok_or_else(|| "binding is not a local let statement".to_string())?;
    let hir::PatKind::Binding(_, binding_hir_id, ident, None) = local.pat.kind else {
        return Err("binding pattern is not a plain identifier".into());
    };
    if binding_hir_id != intent.binding_hir_id
        || ident.name.as_str() != intent.binding_name
        || !matches!(local.source, hir::LocalSource::Normal)
        || local.els.is_some()
    {
        return Err("binding is not a simple local let binding".into());
    }
    let init = local.init.ok_or_else(|| "binding has no initializer".to_string())?;
    if local.span.from_expansion() || local.pat.span.from_expansion() || init.span.from_expansion()
    {
        return Err("binding or initializer comes from a macro expansion".into());
    }

    let typeck = tcx.typeck(intent.body_owner);
    let mut edits = initializer_edits(tcx, typeck, intent.source, strategy, init)?;
    if let Some(ty) = local.ty {
        edits.push(type_annotation_edit(tcx, intent.source, strategy, ty)?);
    }

    let mut collector = LocalUseCollector {
        binding_hir_id: intent.binding_hir_id,
        uses: Vec::new(),
        saw_closure: false,
    };
    collector.visit_expr(body.value);
    if collector.saw_closure {
        return Err(
            "function contains a closure or async block; capture analysis is not rewritten".into(),
        );
    }
    if collector.uses.is_empty() {
        return Err("binding has no expression uses to rewrite".into());
    }

    for use_expr in collector.uses {
        if use_expr.span.from_expansion() {
            return Err("binding use comes from a macro expansion".into());
        }
        classify_use(tcx, typeck, use_expr, strategy, &mut edits)?;
    }
    edits.sort_by_key(|edit| (edit.span.lo(), edit.span.hi()));
    Ok(edits)
}

fn is_non_async_function(tcx: TyCtxt<'_>, owner: hir::def_id::LocalDefId) -> bool {
    match tcx.hir_node_by_def_id(owner) {
        hir::Node::Item(hir::Item { kind: hir::ItemKind::Fn { sig, .. }, .. }) => {
            !sig.header.asyncness.is_async()
        }
        hir::Node::ImplItem(hir::ImplItem { kind: hir::ImplItemKind::Fn(sig, _), .. }) => {
            !sig.header.asyncness.is_async()
        }
        hir::Node::TraitItem(hir::TraitItem {
            kind: hir::TraitItemKind::Fn(sig, hir::TraitFn::Provided(_)),
            ..
        }) => !sig.header.asyncness.is_async(),
        _ => false,
    }
}

fn initializer_edits(
    tcx: TyCtxt<'_>,
    typeck: &rustc_middle::ty::TypeckResults<'_>,
    source: BorrowckWrapperSource,
    strategy: BorrowckWrapperStrategy,
    init: &hir::Expr<'_>,
) -> Result<Vec<BorrowckRepairEdit>, String> {
    if source == BorrowckWrapperSource::Plain {
        let constructor = match strategy {
            BorrowckWrapperStrategy::RefCell => "::std::cell::RefCell::new(",
            BorrowckWrapperStrategy::Mutex => "::std::sync::Mutex::new(",
            BorrowckWrapperStrategy::RwLock => "::std::sync::RwLock::new(",
            _ => return Err("plain bindings require a bare interior-mutability wrapper".into()),
        };
        return Ok(vec![
            BorrowckRepairEdit { span: init.span.shrink_to_lo(), replacement: constructor.into() },
            BorrowckRepairEdit { span: init.span.shrink_to_hi(), replacement: ")".into() },
        ]);
    }

    let hir::ExprKind::Call(callee, [_inner]) = init.kind else {
        return Err(match source {
            BorrowckWrapperSource::Box => "Box initializer is not exactly Box::new(value)",
            BorrowckWrapperSource::Rc => "Rc initializer is not exactly Rc::new(value)",
            BorrowckWrapperSource::Plain => unreachable!(),
        }
        .into());
    };
    let hir::ExprKind::Path(qpath) = callee.kind else {
        return Err("wrapper initializer constructor is not a resolved path".into());
    };
    let Res::Def(_, constructor) = typeck.qpath_res(&qpath, callee.hir_id) else {
        return Err("wrapper initializer constructor did not resolve".into());
    };
    let expected_parent = match source {
        BorrowckWrapperSource::Box => tcx.lang_items().owned_box(),
        BorrowckWrapperSource::Rc => tcx.get_diagnostic_item(sym::Rc),
        BorrowckWrapperSource::Plain => unreachable!(),
    };
    let constructor_parent = tcx
        .inherent_impl_of_assoc(constructor)
        .and_then(|impl_id| {
            tcx.type_of(impl_id).instantiate_identity().skip_norm_wip().ty_adt_def()
        })
        .map(|adt| adt.did());
    if constructor_parent != expected_parent || tcx.item_name(constructor) != sym::new {
        return Err(match source {
            BorrowckWrapperSource::Box => "Box initializer is not exactly Box::new(value)",
            BorrowckWrapperSource::Rc => "Rc initializer is not exactly Rc::new(value)",
            BorrowckWrapperSource::Plain => unreachable!(),
        }
        .into());
    }

    let replacement = match strategy {
        BorrowckWrapperStrategy::Arc => "::std::sync::Arc::new",
        BorrowckWrapperStrategy::ArcMutex => "::std::sync::Arc::new(::std::sync::Mutex::new",
        BorrowckWrapperStrategy::ArcRwLock => "::std::sync::Arc::new(::std::sync::RwLock::new",
        BorrowckWrapperStrategy::Rc => "::std::rc::Rc::new",
        BorrowckWrapperStrategy::RcRefCell => "::std::rc::Rc::new(::std::cell::RefCell::new",
        BorrowckWrapperStrategy::RefCell
        | BorrowckWrapperStrategy::Mutex
        | BorrowckWrapperStrategy::RwLock => {
            return Err("Box and Rc bindings require a shared-ownership rewrite".into());
        }
    };
    let mut edits = vec![BorrowckRepairEdit { span: callee.span, replacement: replacement.into() }];
    if matches!(
        strategy,
        BorrowckWrapperStrategy::RcRefCell
            | BorrowckWrapperStrategy::ArcMutex
            | BorrowckWrapperStrategy::ArcRwLock
    ) {
        edits.push(BorrowckRepairEdit { span: init.span.shrink_to_hi(), replacement: ")".into() });
    }
    Ok(edits)
}

fn type_annotation_edit(
    tcx: TyCtxt<'_>,
    source: BorrowckWrapperSource,
    strategy: BorrowckWrapperStrategy,
    ty: &hir::Ty<'_>,
) -> Result<BorrowckRepairEdit, String> {
    let source_map = tcx.sess.source_map();
    let replacement = match source {
        BorrowckWrapperSource::Plain => {
            let original = source_map
                .span_to_snippet(ty.span)
                .map_err(|_| "explicit type annotation source is unavailable")?;
            match strategy {
                BorrowckWrapperStrategy::RefCell => {
                    format!("::std::cell::RefCell<{original}>")
                }
                BorrowckWrapperStrategy::Mutex => format!("::std::sync::Mutex<{original}>"),
                BorrowckWrapperStrategy::RwLock => format!("::std::sync::RwLock<{original}>"),
                _ => return Err("plain binding has an incompatible wrapper strategy".into()),
            }
        }
        BorrowckWrapperSource::Box | BorrowckWrapperSource::Rc => {
            let inner = single_type_argument_span(ty).ok_or_else(|| {
                "wrapper type annotation must have exactly one type argument".to_string()
            })?;
            let inner = source_map
                .span_to_snippet(inner)
                .map_err(|_| "wrapper type argument source is unavailable")?;
            match strategy {
                BorrowckWrapperStrategy::Arc => format!("::std::sync::Arc<{inner}>"),
                BorrowckWrapperStrategy::ArcMutex => {
                    format!("::std::sync::Arc<::std::sync::Mutex<{inner}>>")
                }
                BorrowckWrapperStrategy::ArcRwLock => {
                    format!("::std::sync::Arc<::std::sync::RwLock<{inner}>>")
                }
                BorrowckWrapperStrategy::Rc => format!("::std::rc::Rc<{inner}>"),
                BorrowckWrapperStrategy::RcRefCell => {
                    format!("::std::rc::Rc<::std::cell::RefCell<{inner}>>")
                }
                BorrowckWrapperStrategy::RefCell
                | BorrowckWrapperStrategy::Mutex
                | BorrowckWrapperStrategy::RwLock => unreachable!(),
            }
        }
    };
    Ok(BorrowckRepairEdit { span: ty.span, replacement })
}

fn single_type_argument_span(ty: &hir::Ty<'_>) -> Option<rustc_span::Span> {
    let hir::TyKind::Path(hir::QPath::Resolved(_, path)) = ty.kind else {
        return None;
    };
    let args = path.segments.last()?.args?;
    if !args.constraints.is_empty() {
        return None;
    }
    let mut types = args.args.iter().filter_map(|arg| match arg {
        hir::GenericArg::Type(ty) => Some(ty.span),
        _ => None,
    });
    let ty = types.next()?;
    if types.next().is_some() || args.args.len() != 1 { None } else { Some(ty) }
}

fn classify_use(
    tcx: TyCtxt<'_>,
    typeck: &rustc_middle::ty::TypeckResults<'_>,
    use_expr: &hir::Expr<'_>,
    strategy: BorrowckWrapperStrategy,
    edits: &mut Vec<BorrowckRepairEdit>,
) -> Result<(), String> {
    let parent = tcx.parent_hir_node(use_expr.hir_id);
    match parent {
        hir::Node::Expr(hir::Expr {
            kind: hir::ExprKind::MethodCall(_, receiver, _, _), ..
        }) if receiver.hir_id == use_expr.hir_id => {
            if let Some(method) = interior_access(strategy, receiver_is_mutable(typeck, receiver)) {
                edits.push(BorrowckRepairEdit {
                    span: use_expr.span.shrink_to_hi(),
                    replacement: method.into(),
                });
            }
            Ok(())
        }
        hir::Node::LetStmt(local)
            if local.init.is_some_and(|init| init.hir_id == use_expr.hir_id)
                && local.ty.is_none()
                && matches!(local.pat.kind, hir::PatKind::Binding(_, _, _, None)) =>
        {
            if has_shared_ownership(strategy) {
                edits.push(BorrowckRepairEdit {
                    span: use_expr.span.shrink_to_hi(),
                    replacement: ".clone()".into(),
                });
                Ok(())
            } else {
                Err("RefCell binding is moved into another local".into())
            }
        }
        hir::Node::Expr(parent @ hir::Expr { kind: hir::ExprKind::Field(base, _), .. })
            if base.hir_id == use_expr.hir_id =>
        {
            rewrite_place_use(tcx, use_expr, parent, strategy, edits)
        }
        hir::Node::Expr(parent @ hir::Expr { kind: hir::ExprKind::Index(base, _, _), .. })
            if base.hir_id == use_expr.hir_id =>
        {
            rewrite_place_use(tcx, use_expr, parent, strategy, edits)
        }
        hir::Node::Expr(
            parent @ hir::Expr { kind: hir::ExprKind::Unary(hir::UnOp::Deref, base), .. },
        ) if base.hir_id == use_expr.hir_id => {
            rewrite_place_use(tcx, use_expr, parent, strategy, edits)
        }
        _ => Err("binding has an unsupported consuming, escaping, or indirect use".into()),
    }
}

fn rewrite_place_use(
    tcx: TyCtxt<'_>,
    use_expr: &hir::Expr<'_>,
    parent: &hir::Expr<'_>,
    strategy: BorrowckWrapperStrategy,
    edits: &mut Vec<BorrowckRepairEdit>,
) -> Result<(), String> {
    let Some(method) = interior_access(strategy, tcx.hir_is_lhs(parent.hir_id)) else {
        return Ok(());
    };
    edits.push(BorrowckRepairEdit {
        span: use_expr.span.shrink_to_hi(),
        replacement: method.into(),
    });
    Ok(())
}

fn has_shared_ownership(strategy: BorrowckWrapperStrategy) -> bool {
    matches!(
        strategy,
        BorrowckWrapperStrategy::Rc
            | BorrowckWrapperStrategy::RcRefCell
            | BorrowckWrapperStrategy::Arc
            | BorrowckWrapperStrategy::ArcMutex
            | BorrowckWrapperStrategy::ArcRwLock
    )
}

fn interior_access(strategy: BorrowckWrapperStrategy, mutable: bool) -> Option<&'static str> {
    match strategy {
        BorrowckWrapperStrategy::RefCell | BorrowckWrapperStrategy::RcRefCell => {
            Some(if mutable { ".borrow_mut()" } else { ".borrow()" })
        }
        BorrowckWrapperStrategy::Mutex | BorrowckWrapperStrategy::ArcMutex => {
            Some(".lock().unwrap()")
        }
        BorrowckWrapperStrategy::RwLock | BorrowckWrapperStrategy::ArcRwLock => {
            Some(if mutable { ".write().unwrap()" } else { ".read().unwrap()" })
        }
        BorrowckWrapperStrategy::Rc | BorrowckWrapperStrategy::Arc => None,
    }
}

fn receiver_is_mutable(
    typeck: &rustc_middle::ty::TypeckResults<'_>,
    receiver: &hir::Expr<'_>,
) -> bool {
    typeck.expr_adjustments(receiver).iter().any(|adjustment| match adjustment.kind {
        Adjust::Borrow(AutoBorrow::Ref(AutoBorrowMutability::Mut { .. }))
        | Adjust::Borrow(AutoBorrow::RawPtr(hir::Mutability::Mut))
        | Adjust::Borrow(AutoBorrow::Pin(hir::Mutability::Mut)) => true,
        Adjust::Deref(DerefAdjustKind::Overloaded(overloaded)) => {
            overloaded.mutbl == hir::Mutability::Mut
        }
        _ => false,
    })
}

struct LocalUseCollector<'hir> {
    binding_hir_id: hir::HirId,
    uses: Vec<&'hir hir::Expr<'hir>>,
    saw_closure: bool,
}

impl<'hir> Visitor<'hir> for LocalUseCollector<'hir> {
    fn visit_expr(&mut self, expr: &'hir hir::Expr<'hir>) {
        if matches!(expr.kind, hir::ExprKind::Closure(_)) {
            self.saw_closure = true;
            return;
        }
        if let hir::ExprKind::Path(hir::QPath::Resolved(
            None,
            hir::Path { res: Res::Local(hir_id), .. },
        )) = expr.kind
            && *hir_id == self.binding_hir_id
        {
            self.uses.push(expr);
        }
        intravisit::walk_expr(self, expr);
    }
}
