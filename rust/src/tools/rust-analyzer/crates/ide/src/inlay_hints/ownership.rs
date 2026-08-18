use hir::{Access, EditionedFileId, Semantics};
use ide_db::{
    FxHashMap, RootDatabase,
    defs::Definition,
    search::{FileReference, FileReferenceNode},
};
use syntax::{
    AstNode, TextRange,
    ast::{self, HasName},
};

use super::{InlayHint, InlayHintLabel, InlayHintPosition, InlayKind, InlayTooltip, LazyProperty};

// Feature: Ownership Flow
//
// With `rust-analyzer.ownership.enable`, rust-analyzer labels estimated moves, shared and mutable
// borrows, reinitializations, and last uses while the file is being edited. After a Cargo check,
// a compatible compiler replaces those estimates with exact MIR-derived ownership events. Hovering
// a participating variable shows its ownership timeline. The same setting enables lifetime,
// reborrow, closure-capture, and implicit-drop hints, plus compiler-validated `Rc`, `RefCell`,
// `Arc`, `Mutex`, and `RwLock` repair actions for conservative local-binding cases.

#[derive(Default)]
struct Candidate {
    labels: Vec<&'static str>,
    explanations: Vec<&'static str>,
}

pub(super) fn hints(
    acc: &mut Vec<InlayHint>,
    sema: &Semantics<'_, RootDatabase>,
    file_id: EditionedFileId,
    range_limit: Option<TextRange>,
) {
    let file = sema.parse(file_id);
    let mut candidates: FxHashMap<TextRange, Candidate> = FxHashMap::default();

    for ident_pat in file.syntax().descendants().filter_map(ast::IdentPat::cast) {
        if ident_pat.pat().is_some() || ident_pat.name().is_none() {
            continue;
        }
        let Some(local) = sema.to_def(&ident_pat) else { continue };
        let is_copy = local.ty(sema.db).is_copy(sema.db);
        let usages = Definition::Local(local).usages(sema).all();
        let Some(references) = usages.references.get(&file_id) else { continue };

        for reference in references {
            let Some((label, explanation)) = classify(sema, reference, is_copy) else {
                continue;
            };
            add_candidate(&mut candidates, reference.range, label, explanation);
        }

        if !is_copy
            && let Some(last) = references.iter().max_by_key(|reference| reference.range.end())
        {
            add_candidate(
                &mut candidates,
                last.range,
                "last?",
                "Estimated final textual use. The compiler replaces this with MIR control-flow facts after a check.",
            );
        }
    }

    for (range, candidate) in candidates {
        if range_limit.is_some_and(|limit| !limit.contains_range(range)) {
            continue;
        }
        let label = candidate.labels.join(" · ");
        let tooltip = format!(
            "**Estimated ownership flow**\n\n{}\n\nSave the file to replace estimates with compiler-derived MIR events.",
            candidate
                .explanations
                .iter()
                .map(|explanation| format!("- {explanation}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        acc.push(InlayHint {
            range,
            position: InlayHintPosition::After,
            pad_left: true,
            pad_right: false,
            kind: InlayKind::Ownership,
            label: InlayHintLabel::simple(
                label,
                Some(LazyProperty::Computed(InlayTooltip::Markdown(tooltip))),
                None,
            ),
            text_edit: None,
            resolve_parent: None,
        });
    }
}

fn add_candidate(
    candidates: &mut FxHashMap<TextRange, Candidate>,
    range: TextRange,
    label: &'static str,
    explanation: &'static str,
) {
    let candidate = candidates.entry(range).or_default();
    if !candidate.labels.contains(&label) {
        candidate.labels.push(label);
        candidate.explanations.push(explanation);
    }
}

fn classify(
    sema: &Semantics<'_, RootDatabase>,
    reference: &FileReference,
    is_copy: bool,
) -> Option<(&'static str, &'static str)> {
    let FileReferenceNode::NameRef(name_ref) = &reference.name else { return None };
    let path_expr = name_ref.syntax().ancestors().find_map(ast::PathExpr::cast)?;
    if path_expr.path()?.as_single_name_ref().as_ref() != Some(name_ref) {
        return None;
    }
    let path = ast::Expr::PathExpr(path_expr.clone());
    let parent = path_expr.syntax().parent()?;

    if let Some(reference) = ast::RefExpr::cast(parent.clone())
        && reference.expr().as_ref() == Some(&path)
    {
        return if reference.mut_token().is_some() {
            Some(("&mut?", "A mutable borrow appears to start here."))
        } else {
            Some(("&?", "A shared borrow appears to start here."))
        };
    }

    if let Some(method) = ast::MethodCallExpr::cast(parent.clone())
        && method.receiver().as_ref() == Some(&path)
    {
        let function = sema.resolve_method_call(&method)?;
        return match function.self_param(sema.db)?.access(sema.db) {
            Access::Shared => Some(("&?", "The method receiver is shared-borrowed.")),
            Access::Exclusive => Some(("&mut?", "The method receiver is mutably borrowed.")),
            Access::Owned if !is_copy => Some(("move?", "The method consumes its receiver.")),
            Access::Owned => None,
        };
    }

    if let Some(let_stmt) = ast::LetStmt::cast(parent.clone())
        && let_stmt.initializer().as_ref() == Some(&path)
        && !is_copy
    {
        return Some(("move?", "A non-Copy value is moved into this binding."));
    }

    if ast::ArgList::cast(parent.clone()).is_some() && !is_copy {
        return Some(("move?", "A non-Copy value may be moved into this call."));
    }

    if let Some(binary) = ast::BinExpr::cast(parent)
        && matches!(binary.op_kind(), Some(ast::BinaryOp::Assignment { .. }))
        && binary.lhs().as_ref() == Some(&path)
    {
        return Some(("reinit?", "This assignment reinitializes or mutates the binding."));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::super::tests::{DISABLED_CONFIG, check_with_config};

    #[test]
    fn estimates_move_borrows_and_last_use() {
        check_with_config(
            super::super::InlayHintsConfig { ownership_hints: true, ..DISABLED_CONFIG },
            r#"
struct Value;
impl Value {
    fn read(&self) {}
    fn write(&mut self) {}
}
fn main() {
    let mut value = Value;
    value.read();
  //^^^^^ &?
    value.write();
  //^^^^^ &mut?
    let moved = value;
              //^^^^^ move? · last?
    moved.read();
  //^^^^^ &? · last?
}
"#,
        );
    }
}
