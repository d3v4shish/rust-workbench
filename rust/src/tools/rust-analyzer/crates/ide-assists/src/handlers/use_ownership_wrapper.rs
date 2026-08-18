use hir::{Access, AsAssocItem, HasCrate};
use ide_db::{
    defs::{Definition, NameRefClass},
    search::{FileReference, FileReferenceNode, ReferenceCategory},
};
use syntax::{
    AstNode, TextRange,
    ast::{self, HasArgList, HasGenericArgs, HasName},
};

use crate::{AssistContext, AssistId, Assists};

#[derive(Clone, Copy, PartialEq, Eq)]
enum WrapperSource {
    Plain,
    Box,
    Rc,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WrapperStrategy {
    Rc,
    RefCell,
    RcRefCell,
}

enum LocalUse {
    Method { range: TextRange, access: Access },
    Place { range: TextRange, mutable: bool },
    Move { range: TextRange },
}

struct PlannedEdit {
    range: TextRange,
    replacement: String,
}

// Assist: use_ownership_wrapper
//
// Conservatively rewrites a simple local binding to use `Rc`, `RefCell`, or
// `Rc<RefCell<_>>`. This immediate assist is structural and is not a substitute for borrow
// checking; a supported compiler can provide a separately validated quick fix after a check.
pub(crate) fn use_ownership_wrapper(acc: &mut Assists, ctx: &AssistContext<'_, '_>) -> Option<()> {
    if !ctx.config.ownership_wrapper_suggestions {
        return None;
    }

    let (ident_pat, target) = if let Some(ident_pat) = ctx.find_node_at_offset::<ast::IdentPat>()
        && ident_pat.pat().is_none()
        && ident_pat
            .name()
            .is_some_and(|name| name.syntax().text_range().contains_range(ctx.selection_trimmed()))
    {
        let target = ident_pat.name()?.syntax().text_range();
        (ident_pat, target)
    } else {
        let name_ref = ctx.find_node_at_offset::<ast::NameRef>()?;
        let local = match NameRefClass::classify(&ctx.sema, &name_ref)? {
            NameRefClass::Definition(Definition::Local(local), _) => local,
            NameRefClass::FieldShorthand { local_ref, .. } => local_ref,
            _ => return None,
        };
        let source = local.primary_source(ctx.db());
        if source.original_file(ctx.db()) != ctx.file_id() {
            return None;
        }
        (source.into_ident_pat()?, name_ref.syntax().text_range())
    };
    let let_stmt = ident_pat.syntax().parent().and_then(ast::LetStmt::cast)?;
    if let_stmt.let_else().is_some() {
        return None;
    }
    let initializer = let_stmt.initializer()?;
    let function = let_stmt.syntax().ancestors().find_map(ast::Fn::cast)?;
    if function.async_token().is_some()
        || function.syntax().descendants().any(|node| ast::ClosureExpr::can_cast(node.kind()))
    {
        return None;
    }

    let local = ctx.sema.to_def(&ident_pat)?;
    let source_adt = local.ty(ctx.db()).as_adt();
    let adt_name = source_adt.map(|adt| adt.name(ctx.db()));
    let is_alloc_adt = source_adt.is_some_and(|adt| {
        adt.krate(ctx.db())
            .display_name(ctx.db())
            .is_some_and(|name| name.canonical_name().as_str() == "alloc")
    });
    let source = match adt_name.as_ref().map(|name| name.as_str()) {
        Some("Box") if is_alloc_adt => WrapperSource::Box,
        Some("Rc") if is_alloc_adt => WrapperSource::Rc,
        Some("RefCell" | "Cell" | "Mutex" | "RwLock") => return None,
        _ => WrapperSource::Plain,
    };

    let usages = Definition::Local(local).usages(&ctx.sema).all();
    if usages.references.len() != 1 {
        return None;
    }
    let references = usages.references.get(&ctx.file_id())?;
    if references.is_empty() {
        return None;
    }
    let uses = references
        .iter()
        .map(|reference| {
            classify_use(ctx, reference).or_else(|| classify_macro_use_conservatively(reference))
        })
        .collect::<Option<Vec<_>>>()?;

    let has_mutation = uses.iter().any(|usage| match usage {
        LocalUse::Method { access, .. } => *access == Access::Exclusive,
        LocalUse::Place { mutable, .. } => *mutable,
        LocalUse::Move { .. } => false,
    });
    let has_move = uses.iter().any(|usage| matches!(usage, LocalUse::Move { .. }));
    let strategy = match (source, has_mutation, has_move) {
        (WrapperSource::Box, false, true) => WrapperStrategy::Rc,
        (WrapperSource::Box | WrapperSource::Rc, true, _) => WrapperStrategy::RcRefCell,
        (WrapperSource::Plain, true, false) => WrapperStrategy::RefCell,
        _ => return None,
    };

    let mut edits = initializer_edits(ctx, source, source_adt, strategy, &initializer)?;
    if let Some(ty) = let_stmt.ty() {
        edits.push(type_edit(source, strategy, &ty)?);
    }
    for usage in uses {
        match usage {
            LocalUse::Move { range } => {
                if matches!(strategy, WrapperStrategy::Rc | WrapperStrategy::RcRefCell) {
                    edits.push(PlannedEdit {
                        range: TextRange::empty(range.end()),
                        replacement: ".clone()".to_owned(),
                    });
                } else {
                    return None;
                }
            }
            LocalUse::Method { range, access } => {
                if strategy != WrapperStrategy::Rc {
                    let suffix = match access {
                        Access::Shared => ".borrow()",
                        Access::Exclusive => ".borrow_mut()",
                        Access::Owned => return None,
                    };
                    edits.push(PlannedEdit {
                        range: TextRange::empty(range.end()),
                        replacement: suffix.to_owned(),
                    });
                }
            }
            LocalUse::Place { range, mutable } => {
                if strategy != WrapperStrategy::Rc {
                    edits.push(PlannedEdit {
                        range: TextRange::empty(range.end()),
                        replacement: if mutable { ".borrow_mut()" } else { ".borrow()" }.to_owned(),
                    });
                }
            }
        }
    }

    let (id, label) = match strategy {
        WrapperStrategy::Rc => {
            ("use_rc_for_shared_ownership", "Use Rc for shared ownership (unvalidated)")
        }
        WrapperStrategy::RefCell => (
            "use_ref_cell_for_interior_mutability",
            "Use RefCell for interior mutability (unvalidated)",
        ),
        WrapperStrategy::RcRefCell => (
            "use_rc_ref_cell_for_shared_mutability",
            "Use Rc<RefCell<_>> for shared mutable ownership (unvalidated)",
        ),
    };
    acc.add(AssistId::refactor_rewrite(id), label, target, move |builder| {
        for edit in edits {
            builder.replace(edit.range, edit.replacement);
        }
    })
}

fn classify_macro_use_conservatively(reference: &FileReference) -> Option<LocalUse> {
    let FileReferenceNode::NameRef(_) = &reference.name else {
        return None;
    };
    // A local mentioned inside a macro argument is resolved in the expanded syntax tree while
    // `reference.range` still points at the editable token in the user's file. Expansion can hide
    // the original method-call parent, so retain a previewable candidate and let the mandatory
    // rustc validation reject any macro whose actual access is stronger than this conservative
    // classification.
    Some(if reference.category.contains(ReferenceCategory::WRITE) {
        LocalUse::Place { range: reference.range, mutable: true }
    } else {
        LocalUse::Method { range: reference.range, access: Access::Shared }
    })
}

fn classify_use(ctx: &AssistContext<'_, '_>, reference: &FileReference) -> Option<LocalUse> {
    let FileReferenceNode::NameRef(name_ref) = &reference.name else { return None };
    if name_ref.syntax().text_range() != reference.range {
        return None;
    }
    let path_expr = name_ref.syntax().ancestors().find_map(ast::PathExpr::cast)?;
    if path_expr.path()?.as_single_name_ref().as_ref() != Some(name_ref) {
        return None;
    }
    let path = ast::Expr::PathExpr(path_expr.clone());
    let parent = path_expr.syntax().parent()?;

    if let Some(method) = ast::MethodCallExpr::cast(parent.clone())
        && method.receiver().as_ref() == Some(&path)
    {
        let function = ctx.sema.resolve_method_call(&method)?;
        let access = function.self_param(ctx.db())?.access(ctx.db());
        return Some(LocalUse::Method { range: reference.range, access });
    }
    if let Some(field) = ast::FieldExpr::cast(parent.clone())
        && field.expr().as_ref() == Some(&path)
    {
        return Some(LocalUse::Place {
            range: reference.range,
            mutable: reference.category.contains(ReferenceCategory::WRITE),
        });
    }
    if let Some(index) = ast::IndexExpr::cast(parent.clone())
        && index.base().as_ref() == Some(&path)
    {
        return Some(LocalUse::Place {
            range: reference.range,
            mutable: reference.category.contains(ReferenceCategory::WRITE),
        });
    }
    if let Some(prefix) = ast::PrefixExpr::cast(parent.clone())
        && prefix.op_kind() == Some(ast::UnaryOp::Deref)
        && prefix.expr().as_ref() == Some(&path)
    {
        return Some(LocalUse::Place {
            range: reference.range,
            mutable: reference.category.contains(ReferenceCategory::WRITE),
        });
    }
    if let Some(let_stmt) = ast::LetStmt::cast(parent)
        && let_stmt.initializer().as_ref() == Some(&path)
        && let_stmt.ty().is_none()
        && matches!(let_stmt.pat(), Some(ast::Pat::IdentPat(_)))
    {
        return Some(LocalUse::Move { range: reference.range });
    }
    None
}

fn initializer_edits(
    ctx: &AssistContext<'_, '_>,
    source: WrapperSource,
    source_adt: Option<hir::Adt>,
    strategy: WrapperStrategy,
    initializer: &ast::Expr,
) -> Option<Vec<PlannedEdit>> {
    if source == WrapperSource::Plain {
        if strategy != WrapperStrategy::RefCell {
            return None;
        }
        return Some(vec![
            PlannedEdit {
                range: TextRange::empty(initializer.syntax().text_range().start()),
                replacement: "::std::cell::RefCell::new(".to_owned(),
            },
            PlannedEdit {
                range: TextRange::empty(initializer.syntax().text_range().end()),
                replacement: ")".to_owned(),
            },
        ]);
    }

    let ast::Expr::CallExpr(call) = initializer else { return None };
    if call.arg_list()?.args().count() != 1 {
        return None;
    }
    let callee = call.expr()?;
    let ast::Expr::PathExpr(path_expr) = &callee else { return None };
    let hir::PathResolution::Def(hir::ModuleDef::Function(constructor)) =
        ctx.sema.resolve_path(&path_expr.path()?)?
    else {
        return None;
    };
    if constructor.name(ctx.db()).as_str() != "new"
        || constructor.as_assoc_item(ctx.db())?.implementing_ty(ctx.db())?.as_adt() != source_adt
    {
        return None;
    }
    let replacement = match strategy {
        WrapperStrategy::Rc => "::std::rc::Rc::new",
        WrapperStrategy::RcRefCell => "::std::rc::Rc::new(::std::cell::RefCell::new",
        WrapperStrategy::RefCell => return None,
    };
    let mut edits = vec![PlannedEdit {
        range: callee.syntax().text_range(),
        replacement: replacement.to_owned(),
    }];
    if strategy == WrapperStrategy::RcRefCell {
        edits.push(PlannedEdit {
            range: TextRange::empty(initializer.syntax().text_range().end()),
            replacement: ")".to_owned(),
        });
    }
    Some(edits)
}

fn type_edit(
    source: WrapperSource,
    strategy: WrapperStrategy,
    ty: &ast::Type,
) -> Option<PlannedEdit> {
    let replacement = if source == WrapperSource::Plain {
        format!("::std::cell::RefCell<{ty}>")
    } else {
        let ast::Type::PathType(path_ty) = ty else { return None };
        let segment = path_ty.path()?.segments().last()?;
        let args: Vec<_> = segment.generic_arg_list()?.generic_args().collect();
        let [ast::GenericArg::TypeArg(inner)] = &args[..] else { return None };
        let inner = inner.ty()?;
        match strategy {
            WrapperStrategy::Rc => format!("::std::rc::Rc<{inner}>"),
            WrapperStrategy::RcRefCell => {
                format!("::std::rc::Rc<::std::cell::RefCell<{inner}>>")
            }
            WrapperStrategy::RefCell => return None,
        }
    };
    Some(PlannedEdit { range: ty.syntax().text_range(), replacement })
}

#[cfg(test)]
mod tests {
    use crate::tests::{check_assist, check_assist_not_applicable};

    use super::*;

    #[test]
    fn box_to_rc() {
        check_assist(
            use_ownership_wrapper,
            r#"
//- /main.rs crate:main deps:alloc
use alloc::boxed::Box;
struct Values;
fn main() {
    let val$0ues: Box<Values> = Box::new(Values);
    let shared = values;
    let _ = shared.len();
    let _ = values.len();
}

//- /alloc.rs crate:alloc
pub mod boxed {
    pub struct Box<T>(T);
    impl<T> Box<T> {
        pub fn new(value: T) -> Self { Box(value) }
        pub fn len(&self) -> usize { 0 }
    }
}
"#,
            r#"
use alloc::boxed::Box;
struct Values;
fn main() {
    let values: ::std::rc::Rc<Values> = ::std::rc::Rc::new(Values);
    let shared = values.clone();
    let _ = shared.len();
    let _ = values.len();
}

"#,
        );
    }

    #[test]
    fn box_to_rc_is_offered_at_the_failing_use() {
        check_assist(
            use_ownership_wrapper,
            r#"
//- /main.rs crate:main deps:alloc
use alloc::boxed::Box;
struct Values;
fn main() {
    let values: Box<Values> = Box::new(Values);
    let shared = values;
    let _ = shared.len();
    let _ = val$0ues.len();
}

//- /alloc.rs crate:alloc
pub mod boxed {
    pub struct Box<T>(T);
    impl<T> Box<T> {
        pub fn new(value: T) -> Self { Box(value) }
        pub fn len(&self) -> usize { 0 }
    }
}
"#,
            r#"
use alloc::boxed::Box;
struct Values;
fn main() {
    let values: ::std::rc::Rc<Values> = ::std::rc::Rc::new(Values);
    let shared = values.clone();
    let _ = shared.len();
    let _ = values.len();
}

"#,
        );
    }

    #[test]
    fn box_to_rc_keeps_read_only_macro_arguments_previewable() {
        check_assist(
            use_ownership_wrapper,
            r#"
//- /main.rs crate:main deps:alloc
use alloc::boxed::Box;
macro_rules! show { ($value:expr) => { let _ = &$value; } }
struct Values;
fn main() {
    let val$0ues: Box<Values> = Box::new(Values);
    let shared = values;
    let _ = shared.len();
    show!(values.len());
}

//- /alloc.rs crate:alloc
pub mod boxed {
    pub struct Box<T>(T);
    impl<T> Box<T> {
        pub fn new(value: T) -> Self { Box(value) }
        pub fn len(&self) -> usize { 0 }
    }
}
"#,
            r#"
use alloc::boxed::Box;
macro_rules! show { ($value:expr) => { let _ = &$value; } }
struct Values;
fn main() {
    let values: ::std::rc::Rc<Values> = ::std::rc::Rc::new(Values);
    let shared = values.clone();
    let _ = shared.len();
    show!(values.len());
}

"#,
        );
    }

    #[test]
    fn plain_to_ref_cell() {
        check_assist(
            use_ownership_wrapper,
            r#"
struct Values;
impl Values {
    fn push(&mut self) {}
    fn len(&self) -> usize { 0 }
}
fn main() {
    let val$0ues: Values = Values;
    values.push();
    let _ = values.len();
}
"#,
            r#"
struct Values;
impl Values {
    fn push(&mut self) {}
    fn len(&self) -> usize { 0 }
}
fn main() {
    let values: ::std::cell::RefCell<Values> = ::std::cell::RefCell::new(Values);
    values.borrow_mut().push();
    let _ = values.borrow().len();
}
"#,
        );
    }

    #[test]
    fn rc_to_rc_ref_cell() {
        check_assist(
            use_ownership_wrapper,
            r#"
//- /main.rs crate:main deps:alloc
use alloc::rc::Rc;
struct Values;
fn main() {
    let val$0ues: Rc<Values> = Rc::new(Values);
    values.push();
}

//- /alloc.rs crate:alloc
pub mod rc {
    pub struct Rc<T>(T);
    impl<T> Rc<T> {
        pub fn new(value: T) -> Self { Rc(value) }
        pub fn push(&mut self) {}
    }
}
"#,
            r#"
use alloc::rc::Rc;
struct Values;
fn main() {
    let values: ::std::rc::Rc<::std::cell::RefCell<Values>> = ::std::rc::Rc::new(::std::cell::RefCell::new(Values));
    values.borrow_mut().push();
}

"#,
        );
    }

    #[test]
    fn rejects_closure_capture() {
        check_assist_not_applicable(
            use_ownership_wrapper,
            r#"
struct Values;
impl Values { fn push(&mut self) {} }
fn main() {
    let val$0ues = Values;
    let use_it = || values.push();
}
"#,
        );
    }

    #[test]
    fn rejects_user_type_named_box() {
        check_assist_not_applicable(
            use_ownership_wrapper,
            r#"
struct Box<T>(T);
impl<T> Box<T> {
    fn new(value: T) -> Self { Box(value) }
    fn len(&self) -> usize { 0 }
}
fn main() {
    let val$0ues: Box<i32> = Box::new(1);
    let shared = values;
    let _ = shared.len();
    let _ = values.len();
}
"#,
        );
    }
}
