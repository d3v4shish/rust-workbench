use hir::{Access, CallableKind, Function, HirDisplay, Semantics};
use ide_db::{FilePosition, FxHashSet, RootDatabase, documentation::HasDocs};
use syntax::{
    AstNode, SourceFile, TextRange, TextSize,
    ast::{self},
};

const MAX_CALLS: usize = 16;
const MAX_BODY_DEPTH: usize = 4;
const MAX_BODY_FUNCTIONS: usize = 64;
const MAX_EFFECTS: usize = 256;
const MAX_DOC_CHARS: usize = 600;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnershipCallAlternative {
    pub name: String,
    pub signature: String,
    pub access: String,
    pub behavior: String,
    pub difference: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnershipCallInsight {
    pub range: TextRange,
    pub name: String,
    pub signature: String,
    pub receiver_type: Option<String>,
    pub required_access: String,
    pub available_access: String,
    pub why_required: String,
    pub documentation: Option<String>,
    pub effects: Vec<String>,
    pub effect_facts: Vec<OwnershipCallEffect>,
    pub call_chain: Vec<String>,
    pub alternatives: Vec<OwnershipCallAlternative>,
    pub provenance: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnershipCallEffect {
    pub kind: String,
    pub summary: String,
    pub certainty: String,
}

pub(crate) fn ownership_call_insights(
    db: &RootDatabase,
    position: FilePosition,
) -> Vec<OwnershipCallInsight> {
    ownership_call_insights_for_positions(db, &[position])
}

pub(crate) fn ownership_call_insights_for_positions(
    db: &RootDatabase,
    positions: &[FilePosition],
) -> Vec<OwnershipCallInsight> {
    let Some(first) = positions.first() else { return Vec::new() };
    let sema = Semantics::new(db);
    let file = sema.parse_guess_edition(first.file_id);
    let mut calls = positions
        .iter()
        .filter(|position| position.file_id == first.file_id)
        .flat_map(|position| calls_at_position(&file, position.offset))
        .collect::<Vec<_>>();
    calls.sort_by_key(|call| {
        let range = call.range();
        (u32::from(range.start()), u32::from(range.end()))
    });
    calls.dedup_by_key(|call| {
        let range = call.range();
        (u32::from(range.start()), u32::from(range.end()))
    });
    calls
        .into_iter()
        .take(MAX_CALLS * positions.len().min(64))
        .filter_map(|call| explain_call(&sema, call))
        .collect()
}

fn calls_at_position(file: &SourceFile, offset: TextSize) -> Vec<CallSyntax> {
    // Compiler diagnostics commonly start exactly at the first byte of a receiver. At that
    // boundary `left_biased()` selects the indentation whitespace, which has no call ancestor,
    // while the right token is the receiver (`self` in `self.events.push(...)`). Inspect both
    // boundary tokens and prefer source tokens over trivia so the diagnostic position and an
    // interactive cursor position resolve the same call.
    let mut tokens = file.syntax().token_at_offset(offset).collect::<Vec<_>>();
    tokens.sort_by_key(|token| token.kind().is_trivia());

    let mut calls = Vec::new();
    for token in tokens {
        let mut token_calls = token
            .parent_ancestors()
            .filter_map(|node| {
                ast::MethodCallExpr::cast(node.clone())
                    .map(CallSyntax::Method)
                    .or_else(|| ast::CallExpr::cast(node).map(CallSyntax::Function))
            })
            .collect::<Vec<_>>();
        if token_calls.is_empty()
            && let Some(statement) = token.parent_ancestors().find(|node| {
                ast::ExprStmt::can_cast(node.kind()) || ast::LetStmt::can_cast(node.kind())
            })
        {
            token_calls.extend(statement.descendants().filter_map(|node| {
                ast::MethodCallExpr::cast(node.clone())
                    .map(CallSyntax::Method)
                    .or_else(|| ast::CallExpr::cast(node).map(CallSyntax::Function))
            }));
        }
        calls.extend(token_calls);
    }

    calls.sort_by_key(|call| call.range().len());
    calls.dedup_by_key(|call| call.range());
    calls.into_iter().take(MAX_CALLS).collect()
}

#[derive(Clone)]
enum CallSyntax {
    Method(ast::MethodCallExpr),
    Function(ast::CallExpr),
}

impl CallSyntax {
    fn range(&self) -> TextRange {
        match self {
            CallSyntax::Method(call) => call.syntax().text_range(),
            CallSyntax::Function(call) => call.syntax().text_range(),
        }
    }
}

fn explain_call(
    sema: &Semantics<'_, RootDatabase>,
    call: CallSyntax,
) -> Option<OwnershipCallInsight> {
    let db = sema.db;
    let (function, receiver, range) = match &call {
        CallSyntax::Method(call) => {
            (sema.resolve_method_call(call)?, call.receiver(), call.syntax().text_range())
        }
        CallSyntax::Function(call) => {
            let callable = sema.resolve_expr_as_callable(&call.expr()?)?;
            let CallableKind::Function(function) = callable.kind() else { return None };
            (function, None, call.syntax().text_range())
        }
    };
    let display_target = function.module(db).krate(db).to_display_target(db);
    let name = function.name(db).display(db, display_target.edition).to_string();
    let signature = function.display(db, display_target).to_string();
    let receiver_type = receiver
        .as_ref()
        .and_then(|receiver| sema.type_of_expr(receiver))
        .map(|ty| ty.original.display(db, display_target).to_string());
    let required_access = required_access(function, db);
    let available_access = available_access(&call, receiver_type.as_deref(), &required_access);
    let why_required = access_explanation(&required_access, receiver_type.as_deref(), &name);
    let documentation =
        function.docs(db).map(|docs| bounded_first_paragraph(docs.as_str(), MAX_DOC_CHARS));
    let mut seen = FxHashSet::default();
    let mut effects = Vec::new();
    let mut call_chain = vec![name.clone()];
    let mut truncated = false;
    summarize_function_body(
        sema,
        function,
        0,
        &mut seen,
        &mut effects,
        &mut call_chain,
        &mut truncated,
    );
    if effects.is_empty() {
        effects.extend(contract_effects(&required_access, function.is_async(db)));
    }
    if let Some(effect) = known_effect(receiver_type.as_deref(), &name) {
        effects.insert(0, effect.to_owned());
    }
    effects.dedup();
    effects.truncate(MAX_EFFECTS);
    let catalog_effect = known_effect(receiver_type.as_deref(), &name);
    let effect_facts = effects
        .iter()
        .map(|effect| OwnershipCallEffect {
            kind: effect_kind(effect, &required_access).to_owned(),
            summary: effect.clone(),
            certainty: if catalog_effect == Some(effect.as_str()) {
                "trusted_standard_library_catalog"
            } else if function.module(db).krate(db).origin(db).is_local() {
                "workspace_source_analysis"
            } else {
                "signature_contract"
            }
            .to_owned(),
        })
        .collect();

    Some(OwnershipCallInsight {
        range,
        name: name.clone(),
        signature,
        receiver_type: receiver_type.clone(),
        required_access,
        available_access,
        why_required,
        documentation,
        effects,
        effect_facts,
        call_chain,
        alternatives: known_alternatives(receiver_type.as_deref(), &name),
        provenance: if function.module(db).krate(db).origin(db).is_local() {
            "signature_and_workspace_body".to_owned()
        } else {
            "signature_docs_and_trusted_catalog".to_owned()
        },
        truncated,
    })
}

fn effect_kind(effect: &str, required_access: &str) -> &'static str {
    let effect = effect.to_ascii_lowercase();
    if effect.contains("allocate") || effect.contains("capacity") || effect.contains("reallocate") {
        "allocation"
    } else if effect.contains("drop") || effect.contains("remove") {
        "destruction"
    } else if effect.contains("block") || effect.contains("lock") || effect.contains("atomic") {
        "synchronization"
    } else if effect.contains("panic") || effect.contains("runtime") {
        "runtime_check"
    } else if effect.contains("consume") || required_access == "move" {
        "ownership_transfer"
    } else if effect.contains("borrow") || required_access.ends_with("borrow") {
        "borrow"
    } else if effect.contains("mutat") || effect.contains("assign") || effect.contains("replace") {
        "mutation"
    } else {
        "behavior"
    }
}

fn required_access(function: Function, db: &RootDatabase) -> String {
    if let Some(self_param) = function.self_param(db) {
        return match self_param.access(db) {
            Access::Shared => "shared_borrow",
            Access::Exclusive => "mutable_borrow",
            Access::Owned => "move",
        }
        .to_owned();
    }
    "call_arguments".to_owned()
}

fn available_access(call: &CallSyntax, receiver_type: Option<&str>, required: &str) -> String {
    if required != "mutable_borrow" {
        return "The call site can supply this contract if each argument satisfies its parameter type."
            .to_owned();
    }
    let receiver_text = match call {
        CallSyntax::Method(call) => call.receiver().map(|expr| expr.syntax().text().to_string()),
        CallSyntax::Function(_) => None,
    }
    .unwrap_or_default();
    if receiver_type.is_some_and(|ty| ty.starts_with("Rc<") || ty.starts_with("Arc<")) {
        return "The receiver is a shared owner. It dereferences for reading but does not provide DerefMut to the inner value."
            .to_owned();
    }
    if receiver_text.starts_with("self.") || receiver_text == "self" {
        return "Access is rooted at `self`; an `&self` method supplies only shared access, while `&mut self` can supply the required exclusive borrow."
            .to_owned();
    }
    "This operation needs an exclusive mutable path from the binding through every wrapper to the receiver."
        .to_owned()
}

fn access_explanation(required: &str, receiver_type: Option<&str>, name: &str) -> String {
    match required {
        "shared_borrow" => format!(
            "`{name}` takes `&self`: it borrows {} for shared access. Interior-mutability types may still change runtime-managed state.",
            receiver_type.unwrap_or("the receiver")
        ),
        "mutable_borrow" => format!(
            "`{name}` takes `&mut self`: the operation may change the observable state of {}, so Rust requires exclusive access for the call.",
            receiver_type.unwrap_or("the receiver")
        ),
        "move" => format!(
            "`{name}` takes ownership of its receiver, so the old receiver place is unavailable after the call unless its type is Copy."
        ),
        _ => "Each argument follows its declared parameter contract: `&T` reads, `&mut T` borrows exclusively, and `T` receives ownership."
            .to_owned(),
    }
}

fn contract_effects(required: &str, is_async: bool) -> Vec<String> {
    let mut effects = vec![match required {
        "shared_borrow" => "May read through a shared borrow; the signature alone does not prove logical immutability.",
        "mutable_borrow" => "May mutate through an exclusive borrow.",
        "move" => "Consumes the receiver and may run Drop for owned state.",
        _ => "Effects depend on the ownership contracts of the call arguments.",
    }
    .to_owned()];
    if is_async {
        effects.push(
            "Calling this async function constructs a future; body effects occur when that future is polled."
                .to_owned(),
        );
    }
    effects
}

fn summarize_function_body(
    sema: &Semantics<'_, RootDatabase>,
    function: Function,
    depth: usize,
    seen: &mut FxHashSet<Function>,
    effects: &mut Vec<String>,
    call_chain: &mut Vec<String>,
    truncated: &mut bool,
) {
    if depth >= MAX_BODY_DEPTH || seen.len() >= MAX_BODY_FUNCTIONS || effects.len() >= MAX_EFFECTS {
        *truncated = true;
        return;
    }
    if !function.module(sema.db).krate(sema.db).origin(sema.db).is_local() || !seen.insert(function)
    {
        return;
    }
    let Some(source) = sema.source(function) else { return };
    let Some(body) = source.value.body() else { return };
    for node in body.syntax().descendants() {
        if effects.len() >= MAX_EFFECTS {
            *truncated = true;
            break;
        }
        if let Some(binary) = ast::BinExpr::cast(node.clone())
            && matches!(binary.op_kind(), Some(ast::BinaryOp::Assignment { .. }))
        {
            effects
                .push("May assign to or replace a place in this workspace-local body.".to_owned());
        }
        if let Some(reference) = ast::RefExpr::cast(node.clone())
            && reference.mut_token().is_some()
        {
            effects.push(
                "May create an exclusive mutable borrow in this workspace-local body.".to_owned(),
            );
        }
        if let Some(method_call) = ast::MethodCallExpr::cast(node.clone())
            && let Some(callee) = sema.resolve_method_call(&method_call)
        {
            let callee_name = callee
                .name(sema.db)
                .display(sema.db, callee.module(sema.db).krate(sema.db).edition(sema.db))
                .to_string();
            if let Some(receiver) = method_call.receiver()
                && let Some(ty) = sema.type_of_expr(&receiver)
                && let Some(effect) = known_effect(
                    Some(
                        &ty.original
                            .display(
                                sema.db,
                                callee.module(sema.db).krate(sema.db).to_display_target(sema.db),
                            )
                            .to_string(),
                    ),
                    &callee_name,
                )
            {
                effects.push(format!("Via `{callee_name}`: {effect}"));
            }
            if callee.module(sema.db).krate(sema.db).origin(sema.db).is_local() {
                call_chain.push(callee_name);
                summarize_function_body(
                    sema,
                    callee,
                    depth + 1,
                    seen,
                    effects,
                    call_chain,
                    truncated,
                );
            }
        }
        if let Some(call) = ast::CallExpr::cast(node)
            && let Some(expr) = call.expr()
            && let Some(callable) = sema.resolve_expr_as_callable(&expr)
            && let CallableKind::Function(callee) = callable.kind()
            && callee.module(sema.db).krate(sema.db).origin(sema.db).is_local()
        {
            let callee_name = callee
                .name(sema.db)
                .display(sema.db, callee.module(sema.db).krate(sema.db).edition(sema.db))
                .to_string();
            call_chain.push(callee_name);
            summarize_function_body(sema, callee, depth + 1, seen, effects, call_chain, truncated);
        }
    }
}

fn known_effect(receiver_type: Option<&str>, name: &str) -> Option<&'static str> {
    let receiver = receiver_type.unwrap_or_default();
    match name {
        "clear" if receiver.contains("Vec<") || receiver.contains("VecDeque<") => Some(
            "Removes and drops every element, sets length to zero, and retains the collection's allocated capacity.",
        ),
        "clear" if receiver.contains("String") => Some(
            "Removes all text, sets the byte length to zero, and retains the String allocation for reuse.",
        ),
        "clear" if receiver.contains("Map<") || receiver.contains("Set<") => Some(
            "Removes and drops every entry while retaining allocation according to the collection implementation.",
        ),
        "push" | "push_back" | "push_front" => Some(
            "Adds one element and may reallocate if the collection has insufficient spare capacity.",
        ),
        "extend" | "append" => Some(
            "Adds multiple elements and may grow the destination allocation; `append` also empties its mutable source collection.",
        ),
        "pop" | "pop_back" | "pop_front" => Some(
            "Removes at most one element and returns it as an Option without shrinking capacity.",
        ),
        "truncate" => Some("Drops elements after the requested length and retains capacity."),
        "resize" | "resize_with" if receiver.contains("Vec<") => Some(
            "Changes the vector length by dropping tail elements or constructing new elements; growth may reallocate.",
        ),
        "reserve" | "reserve_exact" => Some(
            "Ensures additional capacity and may allocate or reallocate without changing the logical elements.",
        ),
        "shrink_to_fit" | "shrink_to" => Some(
            "Requests a smaller allocation while preserving elements; the allocator may retain extra capacity.",
        ),
        "split_off" if receiver.contains("Vec<") || receiver.contains("String") => Some(
            "Keeps the prefix in the original value and returns ownership of an independently allocated suffix.",
        ),
        "sort"
        | "sort_by"
        | "sort_by_key"
        | "sort_unstable"
        | "sort_unstable_by"
        | "sort_unstable_by_key" => Some(
            "Reorders elements in place; stable variants may allocate and unstable variants do not preserve equal-element order.",
        ),
        "dedup" | "dedup_by" | "dedup_by_key" => Some(
            "Removes consecutive duplicate elements in place and drops removed values without shrinking capacity.",
        ),
        "retain" => {
            Some("Keeps only elements accepted by a predicate and drops the others in place.")
        }
        "drain" => Some(
            "Removes a range and yields the removed elements; its mutable borrow remains active while the drain iterator is live.",
        ),
        "insert" => Some("Adds or replaces an element and may allocate or move existing elements."),
        "entry" if receiver.contains("Map<") => Some(
            "Returns a vacant-or-occupied entry handle that keeps exclusive access to the map for the handle's lifetime.",
        ),
        "remove" => Some(
            "Removes an element or entry and returns ownership of the removed value when present.",
        ),
        "swap_remove" if receiver.contains("Vec<") => Some(
            "Removes and returns one element in constant time by moving the final element into its slot; order changes.",
        ),
        "swap" if receiver.contains("Vec<") || receiver.contains("slice") => {
            Some("Exchanges two elements in place without allocating or changing length.")
        }
        "len" | "is_empty" | "capacity" => {
            Some("Observes collection metadata without changing its contents.")
        }
        "iter" | "get" | "first" | "last" | "contains" => {
            Some("Creates shared access to existing contents without transferring ownership.")
        }
        "iter_mut" | "get_mut" | "first_mut" | "last_mut"
            if !receiver.contains("RefCell<") && !receiver.contains("Mutex<") =>
        {
            Some(
                "Creates exclusive mutable access to part of the collection for the returned borrow's lifetime.",
            )
        }
        "as_slice" | "as_str" | "as_bytes" | "as_ref" => {
            Some("Creates a borrowed view of existing storage without transferring ownership.")
        }
        "into_iter" | "into_inner" => Some(
            "Consumes the wrapper or collection and transfers ownership of the contained value or yielded elements.",
        ),
        "take" if receiver.contains("Option<") || receiver.contains("RefCell<") => Some(
            "Replaces the stored value with its default empty state and returns ownership of the previous value.",
        ),
        "replace" if receiver.contains("Option<") || receiver.contains("RefCell<") => Some(
            "Replaces the stored value and returns ownership of the previous value, using the wrapper's access checks.",
        ),
        "map" | "and_then" | "or_else"
            if receiver.contains("Option<") || receiver.contains("Result<") =>
        {
            Some(
                "Consumes the Option or Result and conditionally transforms its contained owned value while preserving the empty or error path.",
            )
        }
        "unwrap" | "expect" if receiver.contains("Option<") || receiver.contains("Result<") => {
            Some(
                "Consumes the wrapper and returns the success value, panicking on the empty or error variant.",
            )
        }
        "borrow" if receiver.contains("RefCell<") => Some(
            "Performs a runtime shared-borrow check and returns a guard; it panics if a mutable borrow is active.",
        ),
        "borrow_mut" if receiver.contains("RefCell<") => Some(
            "Performs a runtime exclusive-borrow check and returns a mutable guard; it panics if any borrow is active.",
        ),
        "try_borrow" if receiver.contains("RefCell<") => Some(
            "Performs a runtime shared-borrow check and returns an error instead of panicking on conflict.",
        ),
        "try_borrow_mut" if receiver.contains("RefCell<") => Some(
            "Performs a runtime exclusive-borrow check and returns an error instead of panicking on conflict.",
        ),
        "get_mut" if receiver.contains("RefCell<") || receiver.contains("Mutex<") => Some(
            "Uses an ordinary exclusive borrow of the wrapper to access the inner value without a runtime borrow or lock check.",
        ),
        "lock" if receiver.contains("Mutex<") => Some(
            "Waits for exclusive runtime access and returns a guard; the call may block and may report poisoning.",
        ),
        "try_lock" if receiver.contains("Mutex<") => Some(
            "Attempts exclusive runtime access without waiting and reports contention or poisoning.",
        ),
        "read" if receiver.contains("RwLock<") => Some(
            "Waits for shared runtime access and returns a read guard; the call may block and may report poisoning.",
        ),
        "write" if receiver.contains("RwLock<") => Some(
            "Waits for exclusive runtime access and returns a write guard; the call may block and may report poisoning.",
        ),
        "clone" if receiver.contains("Rc<") => Some("Adds one non-atomic shared-owner count."),
        "clone" if receiver.contains("Arc<") => Some("Adds one atomic shared-owner count."),
        "downgrade" if receiver.contains("Rc<") || receiver.contains("Arc<") => Some(
            "Creates a weak non-owning handle and increments the weak count without keeping the value alive.",
        ),
        "upgrade" if receiver.contains("Weak<") => Some(
            "Attempts to create a strong shared owner and returns None when the allocation's value was already dropped.",
        ),
        "strong_count" | "weak_count" if receiver.contains("Rc<") || receiver.contains("Arc<") => {
            Some(
                "Reads a momentary reference-count value; concurrent Arc counts may change immediately afterward.",
            )
        }
        "make_mut" if receiver.contains("Rc<") || receiver.contains("Arc<") => Some(
            "Provides mutable access using clone-on-write when other strong owners exist; this may clone the inner value.",
        ),
        "next" | "next_back" => Some(
            "Mutates iterator state and returns ownership of or a borrow to the next item according to the iterator's Item type.",
        ),
        "collect" => Some(
            "Consumes an iterator and builds a destination collection selected by the expected type; allocation depends on that collection.",
        ),
        "push_str" if receiver.contains("String") => Some(
            "Copies UTF-8 bytes into the String and may reallocate when spare capacity is insufficient.",
        ),
        "replace_range" if receiver.contains("String") => Some(
            "Replaces a UTF-8 boundary-aligned byte range in place and may move bytes or reallocate.",
        ),
        _ => None,
    }
}

fn known_alternatives(receiver_type: Option<&str>, name: &str) -> Vec<OwnershipCallAlternative> {
    let receiver = receiver_type.unwrap_or_default();
    let rows: &[(&str, &str, &str, &str)] = match name {
        "clear" if receiver.contains("Vec<") || receiver.contains("VecDeque<") => &[
            (
                "truncate",
                "truncate(n)",
                "mutable_borrow",
                "Keep the first n elements and drop only the tail.",
            ),
            (
                "retain",
                "retain(predicate)",
                "mutable_borrow",
                "Keep elements selected by a predicate.",
            ),
            (
                "drain",
                "drain(range)",
                "mutable_borrow",
                "Remove a range while receiving the removed elements.",
            ),
            ("pop", "pop()", "mutable_borrow", "Remove and return one element at a time."),
            (
                "mem::take",
                "std::mem::take(&mut value)",
                "mutable_borrow",
                "Replace the collection and return the complete old allocation.",
            ),
        ],
        "push" | "push_back" | "push_front" => &[
            ("extend", "extend(iter)", "mutable_borrow", "Add several elements from an iterator."),
            (
                "insert",
                "insert(index, value)",
                "mutable_borrow",
                "Add at a chosen position or key.",
            ),
            (
                "append",
                "append(&mut other)",
                "mutable_borrow",
                "Move every element from another compatible collection.",
            ),
        ],
        "remove" => &[
            (
                "retain",
                "retain(predicate)",
                "mutable_borrow",
                "Remove everything that fails a predicate.",
            ),
            ("drain", "drain(range)", "mutable_borrow", "Remove and iterate over a range."),
            (
                "swap_remove",
                "swap_remove(index)",
                "mutable_borrow",
                "Remove in constant time when order need not be preserved.",
            ),
        ],
        "borrow_mut" if receiver.contains("RefCell<") => &[
            (
                "get_mut",
                "get_mut()",
                "mutable_borrow",
                "Use compile-time exclusive access when the RefCell itself is mutably borrowed.",
            ),
            (
                "try_borrow_mut",
                "try_borrow_mut()",
                "shared_borrow",
                "Return an error instead of panicking on a borrow conflict.",
            ),
            (
                "replace",
                "replace(value)",
                "shared_borrow",
                "Swap the stored value under a runtime borrow check.",
            ),
        ],
        _ => &[],
    };
    rows.iter()
        .map(|(alternative, signature, access, behavior)| OwnershipCallAlternative {
            name: (*alternative).to_owned(),
            signature: (*signature).to_owned(),
            access: (*access).to_owned(),
            behavior: (*behavior).to_owned(),
            difference: format!(
                "Unlike `{name}`, choose this only when that behavior matches the program's intent."
            ),
        })
        .collect()
}

fn bounded_first_paragraph(documentation: &str, max_chars: usize) -> String {
    let paragraph = documentation.split("\n\n").next().unwrap_or(documentation).trim();
    let mut output = paragraph.chars().take(max_chars).collect::<String>();
    if paragraph.chars().count() > max_chars {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use crate::fixture;

    #[test]
    fn explains_mutable_method_contract_effect_and_alternatives() {
        let (analysis, position) = fixture::position(
            r#"
struct Vec<T>(T);
impl<T> Vec<T> {
    /// Removes every item but keeps reusable storage.
    fn clear(&mut self) { }
}
fn main() {
    let mut values = Vec(1_i32);
    values.cl$0ear();
}
"#,
        );
        let insights = analysis.ownership_call_insights(position).unwrap();
        let insight = insights.iter().find(|insight| insight.name == "clear").unwrap();
        assert_eq!(insight.required_access, "mutable_borrow");
        assert!(insight.signature.contains("&mut self"));
        assert!(insight.effects.iter().any(|effect| effect.contains("retains")));
        assert!(insight.effect_facts.iter().any(|effect| {
            effect.kind == "allocation" && effect.certainty == "trusted_standard_library_catalog"
        }));
        assert!(insight.alternatives.iter().any(|alternative| alternative.name == "truncate"));
        assert!(insight.documentation.as_deref().unwrap().contains("Removes every item"));
    }

    #[test]
    fn follows_workspace_local_calls_with_a_strict_bound() {
        let (analysis, position) = fixture::position(
            r#"
struct Store { count: i32 }
impl Store {
    fn reset(&mut self) { self.count = 0; }
    fn reset_for_day(&mut self) { self.reset(); }
}
fn main() {
    let mut store = Store { count: 2 };
    store.reset_for_$0day();
}
"#,
        );
        let insights = analysis.ownership_call_insights(position).unwrap();
        let insight = insights.iter().find(|insight| insight.name == "reset_for_day").unwrap();
        assert_eq!(insight.provenance, "signature_and_workspace_body");
        assert!(insight.call_chain.iter().any(|callee| callee == "reset"));
        assert!(insight.effects.iter().any(|effect| effect.contains("assign")));
    }

    #[test]
    fn distinguishes_shared_and_owned_receiver_contracts() {
        let (analysis, shared_position) = fixture::position(
            r#"
struct Value;
impl Value { fn inspect(&self) {} }
fn main() { Value.ins$0pect(); }
"#,
        );
        let shared = analysis.ownership_call_insights(shared_position).unwrap();
        assert_eq!(shared[0].required_access, "shared_borrow");

        let (analysis, owned_position) = fixture::position(
            r#"
struct Value;
impl Value { fn consume(self) {} }
fn main() { Value.con$0sume(); }
"#,
        );
        let owned = analysis.ownership_call_insights(owned_position).unwrap();
        assert_eq!(owned[0].required_access, "move");
    }

    #[test]
    fn unresolved_calls_do_not_invent_an_explanation() {
        let (analysis, position) = fixture::position("fn main() { missing$0(); }");
        assert!(analysis.ownership_call_insights(position).unwrap().is_empty());
    }

    #[test]
    fn repeated_event_positions_share_one_parse_and_one_call_explanation() {
        let (analysis, position) = fixture::position(
            r#"
struct Store;
impl Store { fn inspect(&self) {} }
fn main() { Store.ins$0pect(); }
"#,
        );
        let insights = analysis.ownership_call_insights_for_positions(vec![position; 64]).unwrap();
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].name, "inspect");
    }

    #[test]
    fn resolves_multiline_method_call_at_receiver_start_boundary() {
        let (analysis, position) = fixture::position(
            r#"
struct Events;
impl Events { fn push(&mut self) {} }
struct Analytics { events: Events }
impl Analytics {
    fn track(&self) {
        $0self.events
            .push();
    }
}
"#,
        );
        let insights = analysis.ownership_call_insights(position).unwrap();
        let push = insights.iter().find(|insight| insight.name == "push").unwrap();
        assert_eq!(push.required_access, "mutable_borrow");
        assert_eq!(push.receiver_type.as_deref(), Some("Events"));
    }
}
