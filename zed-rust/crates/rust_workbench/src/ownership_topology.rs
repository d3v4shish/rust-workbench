use std::collections::{BTreeMap, BTreeSet, VecDeque};

use project::lsp_store::rust_analyzer_ext::{self, OwnershipModel, OwnershipProblem};

use super::{readable_access, readable_available_access, selected_mutation_operation};

pub(super) const TOPOLOGY_CANVAS_WIDTH: u16 = 420;
const TOPOLOGY_NODE_HEIGHT: u16 = 78;
const TOPOLOGY_ROW_START: u16 = 8;
const TOPOLOGY_ROW_STRIDE: u16 = 102;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum TopologyColumn {
    Local,
    Wrapper,
    Target,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct TopologyRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TopologyNode {
    pub id: String,
    pub place: String,
    pub label: String,
    pub type_name: String,
    pub detail: String,
    pub kind: String,
    pub storage: String,
    pub state: String,
    pub provenance: String,
    pub column: TopologyColumn,
    pub range: Option<lsp::Range>,
    pub rect: TopologyRect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TopologyEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub label: String,
    pub provenance: String,
    pub active: bool,
    pub range: Option<lsp::Range>,
    pub route: Vec<(u16, u16)>,
    pub label_position: (u16, u16),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TopologyMoment {
    pub title: String,
    pub explanation: String,
    pub range: lsp::Range,
    pub path_marker: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct OwnershipTopologyScene {
    pub title: String,
    pub summary: String,
    pub nodes: Vec<TopologyNode>,
    pub edges: Vec<TopologyEdge>,
    pub moments: Vec<TopologyMoment>,
    pub selected_step: usize,
    pub access_lines: Vec<String>,
    pub canvas_height: u16,
    pub expanded: bool,
    pub truncated: bool,
    pub legacy_limited: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TeachingDiagramFamily {
    Value,
    Reference,
    Sequence,
    SharedOwner,
    InteriorMutable,
    Lock,
    Conditional,
    Closure,
    TraitObject,
    Async,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedType {
    outer: String,
    arguments: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TeachingLayer {
    id_suffix: String,
    parent_suffix: Option<String>,
    label: String,
    type_name: String,
    detail: String,
    kind: String,
    storage: String,
    relation: String,
    provenance: String,
}

fn simple_type_name(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path).trim()
}

fn split_type_arguments(source: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut depth = 0_u32;
    let mut start = 0;
    for (index, character) in source.char_indices() {
        match character {
            '<' | '(' | '[' => depth = depth.saturating_add(1),
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                arguments.push(source[start..index].trim().to_owned());
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    let tail = source[start..].trim();
    if !tail.is_empty() {
        arguments.push(tail.to_owned());
    }
    arguments
}

fn parse_type(source: &str) -> ParsedType {
    let source = source.trim();
    if let Some(rest) = source.strip_prefix('&') {
        let mut rest = rest.trim_start();
        if rest.starts_with('\'') {
            rest = rest
                .split_once(char::is_whitespace)
                .map_or(rest, |(_, rest)| rest.trim_start());
        }
        let (outer, inner) = rest
            .strip_prefix("mut ")
            .map_or(("&", rest), |inner| ("&mut", inner));
        return ParsedType {
            outer: outer.to_owned(),
            arguments: vec![inner.trim().to_owned()],
        };
    }
    for pointer in ["*const ", "*mut "] {
        if let Some(inner) = source.strip_prefix(pointer) {
            return ParsedType {
                outer: pointer.trim().to_owned(),
                arguments: vec![inner.trim().to_owned()],
            };
        }
    }

    let Some(open) = source.find('<') else {
        return ParsedType {
            outer: simple_type_name(source).to_owned(),
            arguments: Vec::new(),
        };
    };
    let mut depth = 0_u32;
    let mut close = None;
    for (offset, character) in source[open..].char_indices() {
        match character {
            '<' => depth = depth.saturating_add(1),
            '>' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    close = Some(open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        return ParsedType {
            outer: simple_type_name(source).to_owned(),
            arguments: Vec::new(),
        };
    };
    ParsedType {
        outer: simple_type_name(&source[..open]).to_owned(),
        arguments: split_type_arguments(&source[open + 1..close]),
    }
}

fn teaching_diagram_family(
    problem: Option<&OwnershipProblem>,
    type_name: &str,
) -> TeachingDiagramFamily {
    if problem.is_some_and(|problem| {
        matches!(
            problem.category.as_str(),
            "await_outside_async" | "recursive_async_function"
        )
    }) || type_name.contains("Future")
        || type_name.contains("async")
    {
        return TeachingDiagramFamily::Async;
    }
    if problem.is_some_and(|problem| {
        matches!(
            problem.category.as_str(),
            "closure_may_outlive_borrow" | "borrowed_data_escapes"
        )
    }) || type_name.contains("closure")
        || type_name.contains("Fn(")
        || type_name.contains("FnMut")
        || type_name.contains("FnOnce")
    {
        return TeachingDiagramFamily::Closure;
    }
    if type_name.contains("dyn ") {
        return TeachingDiagramFamily::TraitObject;
    }
    match parse_type(type_name).outer.as_str() {
        "Vec" | "String" | "VecDeque" | "BinaryHeap" | "HashMap" | "HashSet" | "BTreeMap"
        | "BTreeSet" | "LinkedList" => TeachingDiagramFamily::Sequence,
        "Rc" | "Arc" | "Weak" | "Box" | "Pin" => TeachingDiagramFamily::SharedOwner,
        "Cell" | "RefCell" | "UnsafeCell" | "OnceCell" | "OnceLock" => {
            TeachingDiagramFamily::InteriorMutable
        }
        "Mutex" | "RwLock" | "MutexGuard" | "RwLockReadGuard" | "RwLockWriteGuard" => {
            TeachingDiagramFamily::Lock
        }
        "Option" | "Result" | "Cow" => TeachingDiagramFamily::Conditional,
        "&" | "&mut" | "*const" | "*mut" => TeachingDiagramFamily::Reference,
        _ => TeachingDiagramFamily::Value,
    }
}

fn teaching_title(family: TeachingDiagramFamily, place: &str) -> String {
    let subject = if place.is_empty() {
        "selected value"
    } else {
        place
    };
    match family {
        TeachingDiagramFamily::Value => format!("How `{subject}` is stored and used"),
        TeachingDiagramFamily::Reference => format!("What `{subject}` points to"),
        TeachingDiagramFamily::Sequence => {
            format!("Collection handle, elements, and references for `{subject}`")
        }
        TeachingDiagramFamily::SharedOwner => {
            format!("Wrapper and owned allocation for `{subject}`")
        }
        TeachingDiagramFamily::InteriorMutable => format!("Runtime access gate inside `{subject}`"),
        TeachingDiagramFamily::Lock => format!("Lock, guard, and protected value for `{subject}`"),
        TeachingDiagramFamily::Conditional => format!("Active representation of `{subject}`"),
        TeachingDiagramFamily::Closure => {
            format!("Closure environment and captured values for `{subject}`")
        }
        TeachingDiagramFamily::TraitObject => {
            format!("Trait-object data and vtable for `{subject}`")
        }
        TeachingDiagramFamily::Async => format!("Future state retained by `{subject}`"),
    }
}

fn push_teaching_layer(
    layers: &mut Vec<TeachingLayer>,
    parent: Option<&str>,
    suffix: impl Into<String>,
    label: impl Into<String>,
    type_name: impl Into<String>,
    detail: impl Into<String>,
    kind: &str,
    storage: &str,
    relation: &str,
    provenance: &str,
) -> String {
    let suffix = suffix.into();
    layers.push(TeachingLayer {
        id_suffix: suffix.clone(),
        parent_suffix: parent.map(str::to_owned),
        label: label.into(),
        type_name: type_name.into(),
        detail: detail.into(),
        kind: kind.to_owned(),
        storage: storage.to_owned(),
        relation: relation.to_owned(),
        provenance: provenance.to_owned(),
    });
    suffix
}

fn append_type_layers(
    type_name: &str,
    parent: &str,
    prefix: &str,
    layers: &mut Vec<TeachingLayer>,
    depth: usize,
) {
    if depth >= 8 || layers.len() >= 12 {
        return;
    }
    let parsed = parse_type(type_name);
    let first_argument = parsed.arguments.first().map(String::as_str).unwrap_or("T");
    let child_prefix = |name: &str| format!("{prefix}:{name}:{depth}");
    match parsed.outer.as_str() {
        "&" | "&mut" => {
            let mutable = parsed.outer == "&mut";
            let handle = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("reference"),
                if mutable {
                    "&mut reference handle"
                } else {
                    "& shared reference handle"
                },
                type_name,
                if mutable {
                    "temporary exclusive access; it does not own the target"
                } else {
                    "temporary read-only access; it does not own the target"
                },
                "handle",
                "inline",
                "stores",
                "compiler_type",
            );
            if first_argument.contains("dyn ") {
                let data = push_teaching_layer(
                    layers,
                    Some(&handle),
                    child_prefix("dyn-data"),
                    "trait-object data",
                    first_argument,
                    "the concrete value behind the trait object",
                    "trait_object_data",
                    "conceptual",
                    "points_to",
                    "derived_from_type",
                );
                push_teaching_layer(
                    layers,
                    Some(&handle),
                    child_prefix("vtable"),
                    "vtable metadata",
                    "method table + layout metadata",
                    "the second word of this fat pointer selects dynamic methods",
                    "metadata",
                    "inline",
                    "dispatches_via",
                    "derived_from_type",
                );
                append_type_layers("concrete T", &data, prefix, layers, depth + 1);
            } else if first_argument.starts_with('[') || first_argument == "str" {
                let target = push_teaching_layer(
                    layers,
                    Some(&handle),
                    child_prefix("slice-data"),
                    "borrowed elements",
                    first_argument,
                    "data pointer target; the reference also stores runtime length metadata",
                    "buffer",
                    "conceptual",
                    "points_to",
                    "derived_from_type",
                );
                push_teaching_layer(
                    layers,
                    Some(&handle),
                    child_prefix("slice-length"),
                    "length metadata",
                    "usize",
                    "the second word of a slice or str reference",
                    "metadata",
                    "inline",
                    "contains",
                    "derived_from_type",
                );
                let _ = target;
            } else {
                push_teaching_layer(
                    layers,
                    Some(&handle),
                    child_prefix("referent"),
                    "borrowed target",
                    first_argument,
                    "the owner keeps this value alive",
                    "borrowed_view",
                    "conceptual",
                    if mutable {
                        "borrow_mutable"
                    } else {
                        "borrow_shared"
                    },
                    "compiler_type",
                );
            }
        }
        "Vec" => {
            let header = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("vec-header"),
                "Vec handle [ptr | len | cap]",
                type_name,
                "stored inline; owns a separately allocated element buffer",
                "wrapper",
                "inline",
                "stores",
                "compiler_type",
            );
            let buffer = push_teaching_layer(
                layers,
                Some(&header),
                child_prefix("vec-buffer"),
                format!("Heap A · [{first_argument} 0] [{first_argument} 1] […]"),
                first_argument,
                "element buffer; capacity is runtime data",
                "buffer",
                "heap",
                "owns_buffer",
                "derived_from_type",
            );
            push_teaching_layer(
                layers,
                Some(&buffer),
                child_prefix("element-zero"),
                format!("{first_argument}[0]"),
                first_argument,
                "the element selected by index 0",
                "element",
                "heap",
                "contains",
                "derived_from_source",
            );
        }
        "VecDeque" | "BinaryHeap" | "HashMap" | "HashSet" | "BTreeMap" | "BTreeSet"
        | "LinkedList" => {
            let header = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("container-header"),
                format!("{} collection state", parsed.outer),
                type_name,
                "inline handle and collection bookkeeping",
                "wrapper",
                "inline",
                "stores",
                "compiler_type",
            );
            push_teaching_layer(
                layers,
                Some(&header),
                child_prefix("container-storage"),
                "owned element storage",
                if parsed.arguments.is_empty() {
                    "elements".to_owned()
                } else {
                    parsed.arguments.join(", ")
                },
                "heap-backed storage whose exact organization depends on the collection",
                "buffer",
                "heap",
                "owns_buffer",
                "derived_from_type",
            );
        }
        "String" => {
            let header = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("string-header"),
                "String handle [ptr | len | cap]",
                type_name,
                "UTF-8 owner stored inline",
                "wrapper",
                "inline",
                "stores",
                "compiler_type",
            );
            push_teaching_layer(
                layers,
                Some(&header),
                child_prefix("string-buffer"),
                "heap UTF-8 bytes",
                "[u8]",
                "owned byte buffer; capacity is runtime data",
                "buffer",
                "heap",
                "owns_buffer",
                "compiler_type",
            );
        }
        "Box" => {
            let dynamic = first_argument.contains("dyn ");
            let handle = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("box-handle"),
                if dynamic {
                    "Box<dyn Trait> fat owner [data | vtable]"
                } else {
                    "Box handle (unique owner)"
                },
                type_name,
                if dynamic {
                    "unique owning data pointer plus dynamic-dispatch metadata"
                } else {
                    "pointer-sized owning handle"
                },
                "handle",
                "inline",
                "stores",
                "compiler_type",
            );
            let allocation = push_teaching_layer(
                layers,
                Some(&handle),
                child_prefix("box-allocation"),
                "Box allocation",
                if dynamic {
                    "erased concrete T"
                } else {
                    first_argument
                },
                if dynamic {
                    "heap allocation containing the concrete value behind dyn Trait"
                } else {
                    "heap allocation containing the owned value"
                },
                "heap_allocation",
                "heap",
                "owns",
                "compiler_type",
            );
            if dynamic {
                push_teaching_layer(
                    layers,
                    Some(&handle),
                    child_prefix("box-vtable"),
                    "vtable metadata",
                    "method pointers + layout + drop",
                    "the metadata word is part of the Box fat pointer, not inside the allocation",
                    "metadata",
                    "inline",
                    "dispatches_via",
                    "derived_from_type",
                );
            } else {
                append_type_layers(first_argument, &allocation, prefix, layers, depth + 1);
            }
        }
        "Rc" | "Arc" => {
            let thread_safe = parsed.outer == "Arc";
            let dynamic = first_argument.contains("dyn ");
            let handle = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("shared-handle"),
                if dynamic {
                    format!(
                        "{}<dyn Trait> fat shared owner [data | vtable]",
                        parsed.outer
                    )
                } else {
                    format!("{} handle (shared owner)", parsed.outer)
                },
                type_name,
                if thread_safe {
                    "atomic reference-counted handle"
                } else {
                    "single-thread reference-counted handle"
                },
                "handle",
                "inline",
                "stores",
                "compiler_type",
            );
            let allocation = push_teaching_layer(
                layers,
                Some(&handle),
                child_prefix("shared-allocation"),
                format!("{} allocation [strong | weak | value]", parsed.outer),
                if dynamic {
                    "erased concrete T"
                } else {
                    first_argument
                },
                if thread_safe {
                    "shared heap allocation with atomic counters"
                } else {
                    "shared heap allocation with non-atomic counters"
                },
                "control_block",
                "heap",
                "shares_allocation",
                "compiler_type",
            );
            if dynamic {
                push_teaching_layer(
                    layers,
                    Some(&handle),
                    child_prefix("shared-vtable"),
                    "vtable metadata",
                    "method pointers + layout + drop",
                    "the metadata travels with each fat shared-owner handle",
                    "metadata",
                    "inline",
                    "dispatches_via",
                    "derived_from_type",
                );
            } else {
                append_type_layers(first_argument, &allocation, prefix, layers, depth + 1);
            }
        }
        "Weak" => {
            let handle = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("weak-handle"),
                "Weak handle (non-owning)",
                type_name,
                "does not keep the value alive",
                "handle",
                "inline",
                "stores",
                "compiler_type",
            );
            push_teaching_layer(
                layers,
                Some(&handle),
                child_prefix("weak-control"),
                "shared control block",
                first_argument,
                "the value may already have been dropped",
                "control_block",
                "heap",
                "weak_reference",
                "compiler_type",
            );
        }
        "Pin" => {
            let constraint = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("pin"),
                "Pin movement constraint",
                type_name,
                "prevents moving the pointee through safe APIs; it is not another allocation",
                "gate",
                "conceptual",
                "wraps",
                "compiler_type",
            );
            append_type_layers(first_argument, &constraint, prefix, layers, depth + 1);
        }
        "Cell" | "RefCell" | "UnsafeCell" | "OnceCell" | "OnceLock" => {
            let wrapper = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("cell"),
                match parsed.outer.as_str() {
                    "RefCell" => "RefCell runtime borrow flag",
                    "Cell" => "Cell copy-in/copy-out gate",
                    "UnsafeCell" => "UnsafeCell mutation boundary",
                    _ => "one-time initialization state",
                },
                type_name,
                match parsed.outer.as_str() {
                    "RefCell" => "tracks shared and mutable borrows at runtime",
                    "Cell" => "permits replacing Copy values through shared access",
                    "UnsafeCell" => "marks the primitive interior-mutability boundary",
                    _ => "tracks whether the inner value has been initialized",
                },
                "borrow_flag",
                "inline",
                "guards_access",
                "compiler_type",
            );
            append_type_layers(first_argument, &wrapper, prefix, layers, depth + 1);
        }
        "Mutex" | "RwLock" => {
            let lock = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("lock"),
                format!("{} runtime lock state", parsed.outer),
                type_name,
                if parsed.outer == "Mutex" {
                    "one writer or blocked waiters"
                } else {
                    "many readers or one writer"
                },
                "lock_state",
                "inline",
                "guards_access",
                "compiler_type",
            );
            append_type_layers(first_argument, &lock, prefix, layers, depth + 1);
        }
        "MutexGuard" | "RwLockReadGuard" | "RwLockWriteGuard" => {
            push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("guard"),
                format!("{} temporary guard", parsed.outer),
                type_name,
                "unlocks or releases access when dropped",
                "guard",
                "inline",
                "points_to",
                "compiler_type",
            );
        }
        "*const" | "*mut" => {
            let handle = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("raw-pointer"),
                format!("{} raw pointer", parsed.outer),
                type_name,
                "non-owning address; dereference requires unsafe and a separate validity proof",
                "handle",
                "inline",
                "stores",
                "compiler_type",
            );
            push_teaching_layer(
                layers,
                Some(&handle),
                child_prefix("raw-target"),
                "possible pointee",
                first_argument,
                "Rust does not infer that this address is live, aligned, or uniquely accessible",
                "borrowed_view",
                "conceptual",
                "points_to",
                "conceptual",
            );
        }
        "Option" | "Result" => {
            let conditional = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("conditional"),
                format!("{} discriminant + active payload", parsed.outer),
                type_name,
                "only the active variant's payload is usable",
                "wrapper",
                "inline",
                "contains",
                "compiler_type",
            );
            for (index, argument) in parsed.arguments.iter().take(2).enumerate() {
                let variant = push_teaching_layer(
                    layers,
                    Some(&conditional),
                    child_prefix(&format!("variant-{index}")),
                    if parsed.outer == "Option" {
                        "Some payload".to_owned()
                    } else if index == 0 {
                        "Ok payload".to_owned()
                    } else {
                        "Err payload".to_owned()
                    },
                    argument,
                    "conditional storage; shown only for the active variant",
                    "inline_value",
                    "inline",
                    "conditional",
                    "compiler_type",
                );
                if index == 0 {
                    append_type_layers(argument, &variant, prefix, layers, depth + 1);
                }
            }
        }
        "Cow" => {
            let borrowed_type = parsed.arguments.last().map(String::as_str).unwrap_or("T");
            let cow = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("cow"),
                "Cow: Borrowed or Owned",
                type_name,
                "clone-on-write chooses a borrowed view or an owned value",
                "wrapper",
                "inline",
                "contains",
                "compiler_type",
            );
            push_teaching_layer(
                layers,
                Some(&cow),
                child_prefix("cow-borrowed"),
                "Borrowed(&T)",
                borrowed_type,
                "non-owning branch",
                "borrowed_view",
                "conceptual",
                "conditional",
                "derived_from_type",
            );
            push_teaching_layer(
                layers,
                Some(&cow),
                child_prefix("cow-owned"),
                "Owned(T::Owned)",
                borrowed_type,
                "owning branch, created when mutation requires it",
                "inline_value",
                "conceptual",
                "conditional",
                "derived_from_type",
            );
        }
        outer if outer.contains("Future") || type_name.contains("async") => {
            let future = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("future"),
                "generated Future state",
                type_name,
                "a state machine value; calling async code does not run it yet",
                "future_state",
                "inline",
                "contains",
                "derived_from_type",
            );
            let locals = push_teaching_layer(
                layers,
                Some(&future),
                child_prefix("saved-locals"),
                "locals kept across .await",
                "captured and live local values",
                "stored inside the future while it is suspended",
                "capture",
                "inline",
                "stores_across_await",
                "conceptual",
            );
            push_teaching_layer(
                layers,
                Some(&locals),
                child_prefix("suspension"),
                "suspension point",
                "Poll::Pending → wake → poll again",
                "execution returns to the executor while the future remains alive",
                "suspension_point",
                "conceptual",
                "suspends_at",
                "conceptual",
            );
        }
        outer if outer.contains("closure") || type_name.contains("closure") => {
            let environment = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("closure"),
                "closure environment",
                type_name,
                "compiler-generated struct containing captured values or references",
                "closure_environment",
                "inline",
                "contains",
                "derived_from_type",
            );
            push_teaching_layer(
                layers,
                Some(&environment),
                child_prefix("captures"),
                "captured values",
                "by &, &mut, or move",
                "capture mode determines Fn, FnMut, or FnOnce behavior",
                "capture",
                "inline",
                "captures",
                "conceptual",
            );
        }
        _ if type_name.contains("dyn ") => {
            let fat = push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("dyn-fat-pointer"),
                "trait-object fat pointer [data | vtable]",
                type_name,
                "two logical pointer components",
                "handle",
                "inline",
                "stores",
                "derived_from_type",
            );
            push_teaching_layer(
                layers,
                Some(&fat),
                child_prefix("dyn-data"),
                "concrete data",
                "erased concrete type",
                "the runtime value implementing the trait",
                "trait_object_data",
                "conceptual",
                "points_to",
                "derived_from_type",
            );
            push_teaching_layer(
                layers,
                Some(&fat),
                child_prefix("dyn-vtable"),
                "vtable",
                "method pointers + size + alignment + drop",
                "used for dynamic dispatch and cleanup",
                "metadata",
                "conceptual",
                "dispatches_via",
                "derived_from_type",
            );
        }
        _ => {
            push_teaching_layer(
                layers,
                Some(parent),
                child_prefix("value"),
                if parsed.outer.is_empty() {
                    "inline value".to_owned()
                } else {
                    format!("{} value", parsed.outer)
                },
                type_name,
                "stored inline at this layer; fields are expanded only when relevant",
                "inline_value",
                "inline",
                "contains",
                "compiler_type",
            );
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BeginnerMemoryStage {
    pub relation_from_previous: Vec<String>,
    pub nodes: Vec<TopologyNode>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct BeginnerMemoryPath {
    pub stages: Vec<BeginnerMemoryStage>,
    pub truncated: bool,
}

pub(super) fn beginner_memory_role(node: &TopologyNode) -> &'static str {
    match node.kind.as_str() {
        "binding" => "Local variable",
        "handle" => "Inline owner handle",
        "control_block" => "Shared heap allocation",
        "borrow_flag" | "gate" => "Runtime borrow gate",
        "buffer" => "Heap element buffer",
        "lock_state" => "Runtime lock state",
        "guard" => "Temporary access guard",
        "wrapper" if node.label.contains("Vec") || node.type_name.starts_with("Vec<") => {
            "Inline collection header"
        }
        "wrapper" => "Inline wrapper",
        "heap_allocation" | "allocation" => "Heap allocation",
        _ if node.storage == "heap" => "Heap value",
        _ if node.storage == "inline" => "Inline value",
        _ => "Value layer",
    }
}

pub(super) fn beginner_relation_label(relation: &str) -> String {
    match relation {
        "stores" => "holds this handle".to_owned(),
        "wraps" => "wraps".to_owned(),
        "owns" => "owns".to_owned(),
        "shares_allocation" => "shares ownership of".to_owned(),
        "weak_reference" => "weakly observes".to_owned(),
        "contains" => "contains".to_owned(),
        "guards_access" => "controls access to".to_owned(),
        "owns_buffer" => "owns the element storage".to_owned(),
        "points_to" => "points to".to_owned(),
        "borrow_shared" => "temporarily reads".to_owned(),
        "borrow_mutable" => "temporarily edits".to_owned(),
        "reborrow" => "borrows again from".to_owned(),
        "moved_to" => "transferred control to".to_owned(),
        other => other.replace('_', " "),
    }
}

#[allow(dead_code)]
pub(super) fn beginner_memory_state(state: &str) -> String {
    if state.contains("reject") || state.contains("invalid") || state.contains("blocked") {
        "Operation blocked here".to_owned()
    } else if state.contains("mutable") || state.contains("exclusive") {
        "Exclusive edit access is active".to_owned()
    } else if state.contains("borrow") || state.contains("read-only") {
        "Temporary read access is active".to_owned()
    } else if state.contains("move") || state.contains("unavailable") {
        "Control moved to another place".to_owned()
    } else if state.contains("drop") || state.contains("dead") {
        "This layer has been cleaned up".to_owned()
    } else if state.contains("available") || state.contains("alive") {
        "Available to use".to_owned()
    } else {
        format!("Current state: {}", state.replace('_', " "))
    }
}

pub(super) fn derive_beginner_memory_path(scene: &OwnershipTopologyScene) -> BeginnerMemoryPath {
    if scene.nodes.is_empty() {
        return BeginnerMemoryPath {
            stages: Vec::new(),
            truncated: scene.truncated,
        };
    }

    let node_order = scene
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut distance = BTreeMap::<String, usize>::new();
    let mut queue = VecDeque::new();
    for node in scene.nodes.iter().filter(|node| node.kind == "binding") {
        distance.insert(node.id.clone(), 0);
        queue.push_back(node.id.clone());
    }
    if queue.is_empty() {
        let Some(first) = scene.nodes.first() else {
            return BeginnerMemoryPath {
                stages: Vec::new(),
                truncated: scene.truncated,
            };
        };
        distance.insert(first.id.clone(), 0);
        queue.push_back(first.id.clone());
    }

    while let Some(source) = queue.pop_front() {
        let Some(source_distance) = distance.get(&source).copied() else {
            continue;
        };
        let mut outgoing = scene
            .edges
            .iter()
            .filter(|edge| edge.source == source)
            .collect::<Vec<_>>();
        outgoing.sort_by(|left, right| {
            relation_priority(&left.label)
                .cmp(&relation_priority(&right.label))
                .then_with(|| {
                    node_order
                        .get(left.target.as_str())
                        .cmp(&node_order.get(right.target.as_str()))
                })
        });
        for edge in outgoing {
            if !distance.contains_key(&edge.target) {
                distance.insert(edge.target.clone(), source_distance.saturating_add(1));
                queue.push_back(edge.target.clone());
            }
        }
    }

    let mut next_distance = distance
        .values()
        .copied()
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    for node in &scene.nodes {
        if !distance.contains_key(&node.id) {
            distance.insert(node.id.clone(), next_distance);
            next_distance = next_distance.saturating_add(1);
        }
    }

    let mut grouped = BTreeMap::<usize, Vec<TopologyNode>>::new();
    for node in &scene.nodes {
        let node_distance = distance.get(&node.id).copied().unwrap_or(usize::MAX);
        grouped.entry(node_distance).or_default().push(node.clone());
    }

    let stages = grouped
        .into_iter()
        .map(|(stage_distance, mut nodes)| {
            nodes.sort_by_key(|node| node_order.get(node.id.as_str()).copied());
            let node_ids = nodes
                .iter()
                .map(|node| node.id.as_str())
                .collect::<BTreeSet<_>>();
            let mut relations = scene
                .edges
                .iter()
                .filter(|edge| {
                    node_ids.contains(edge.target.as_str())
                        && distance
                            .get(&edge.source)
                            .is_some_and(|source_distance| *source_distance < stage_distance)
                })
                .map(|edge| beginner_relation_label(&edge.label))
                .collect::<Vec<_>>();
            relations.sort();
            relations.dedup();
            BeginnerMemoryStage {
                relation_from_previous: relations,
                nodes,
            }
        })
        .collect();

    BeginnerMemoryPath {
        stages,
        truncated: scene.truncated,
    }
}

pub(super) fn topology_column(kind: &str, storage: &str) -> TopologyColumn {
    if storage == "heap"
        || matches!(
            kind,
            "heap_allocation" | "allocation" | "buffer" | "control_block"
        )
    {
        TopologyColumn::Target
    } else if matches!(
        kind,
        "handle"
            | "wrapper"
            | "inline_value"
            | "borrow_flag"
            | "lock_state"
            | "guard"
            | "metadata"
            | "wrapper_state"
            | "gate"
    ) || storage == "inline"
    {
        TopologyColumn::Wrapper
    } else {
        TopologyColumn::Local
    }
}

pub(super) fn topology_column_title(column: TopologyColumn) -> &'static str {
    match column {
        TopologyColumn::Local => "VARIABLE",
        TopologyColumn::Wrapper => "INLINE WRAPPER",
        TopologyColumn::Target => "HEAP / TARGET",
    }
}

pub(super) fn topology_state_at_step(
    model: &OwnershipModel,
    selected_step: usize,
) -> BTreeMap<String, String> {
    let mut states = model
        .memory_graph
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node.state.clone()))
        .collect::<BTreeMap<_, _>>();
    for snapshot in model
        .memory_graph
        .snapshots
        .iter()
        .take(selected_step.saturating_add(1))
    {
        for delta in &snapshot.deltas {
            states.insert(delta.node_id.clone(), delta.to.clone());
        }
    }
    states
}

pub(super) fn topology_edge_active_at_step(
    edge: &rust_analyzer_ext::OwnershipMemoryEdge,
    model: &OwnershipModel,
    selected_step: usize,
) -> bool {
    let snapshots = model
        .memory_graph
        .snapshots
        .iter()
        .take(selected_step.saturating_add(1));
    let created = edge.event_id.as_deref().is_none_or(|event_id| {
        snapshots
            .clone()
            .any(|snapshot| snapshot.event_id == event_id)
    });
    let removed = snapshots.clone().any(|snapshot| {
        snapshot.deltas.iter().any(|delta| {
            delta.relation_removed.as_deref() == Some(edge.relation.as_str())
                && (delta.node_id == edge.source || delta.node_id == edge.target)
        })
    });
    created && !removed
}

fn topology_detail(node: &rust_analyzer_ext::OwnershipMemoryNode) -> String {
    let layout = match (node.size, node.align) {
        (Some(size), Some(align)) => format!("{size} B · align {align}"),
        (Some(size), None) => format!("{size} B"),
        _ => "layout unknown".to_owned(),
    };
    format!("{} · {layout}", node.storage.replace('_', " "))
}

fn topology_moment_explanation(kind: &str, place: &str) -> String {
    match kind {
        "move" | "partial_move" => {
            format!("Ownership leaves `{place}` here; its destination becomes the usable owner.")
        }
        "clone" => format!(
            "A new handle is created from `{place}`; shared allocations are not duplicated."
        ),
        "borrow_shared" => format!("A read-only loan from `{place}` starts here."),
        "borrow_mutable" | "borrow_activate" => {
            format!("An exclusive mutable loan from `{place}` becomes active here.")
        }
        "borrow_end" => format!("The loan from `{place}` ends after its final use."),
        "invalid_use" | "conflict" => {
            format!("Rust rejects this operation because `{place}` lacks the required access.")
        }
        "reinitialize" => format!("A new value makes `{place}` usable again."),
        "drop" => format!("The value owned through `{place}` is destroyed here."),
        _ => format!("The ownership state of `{place}` changes here."),
    }
}

fn relation_priority(relation: &str) -> u8 {
    match relation {
        "stores" => 0,
        "wraps" => 1,
        "owns" | "shares_allocation" | "weak_reference" => 2,
        "contains" | "guards_access" => 3,
        "owns_buffer" | "points_to" => 4,
        "borrow_shared" | "borrow_mutable" | "reborrow" => 5,
        "moved_to" => 6,
        _ => 7,
    }
}

fn place_root(place: &str) -> &str {
    place
        .trim_start_matches('*')
        .split(['.', '['])
        .next()
        .unwrap_or(place)
}

fn semantic_node_order(
    nodes: &[TopologyNode],
    edges: &[TopologyEdge],
    selected_names: &[&str],
    node_limit: usize,
) -> Vec<String> {
    if node_limit == 0 {
        return Vec::new();
    }
    let mut roots = nodes
        .iter()
        .filter(|node| node.kind == "binding")
        .filter(|node| {
            selected_names.iter().any(|selected| {
                place_root(&node.place) == place_root(selected)
                    || node.label.contains(place_root(selected))
            })
        })
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    if roots.is_empty() {
        roots.extend(
            nodes
                .iter()
                .filter(|node| node.kind == "binding")
                .take(1)
                .map(|node| node.id.clone()),
        );
    }
    if roots.is_empty() {
        roots.extend(nodes.first().map(|node| node.id.clone()));
    }

    let mut queue = VecDeque::from(roots);
    let mut selected = Vec::new();
    let mut visited = BTreeSet::new();
    while let Some(id) = queue.pop_front() {
        if !visited.insert(id.clone()) {
            continue;
        }
        selected.push(id.clone());
        if selected.len() >= node_limit {
            break;
        }
        let mut adjacent = edges
            .iter()
            .filter_map(|edge| {
                if edge.source == id {
                    Some((false, relation_priority(&edge.label), edge.target.clone()))
                } else if edge.target == id {
                    Some((true, relation_priority(&edge.label), edge.source.clone()))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        adjacent.sort();
        for (_, _, adjacent_id) in adjacent {
            if !visited.contains(&adjacent_id) {
                queue.push_back(adjacent_id);
            }
        }
    }
    selected
}

fn layout_topology_scene(nodes: &mut [TopologyNode], edges: &mut [TopologyEdge]) -> u16 {
    let row_y = |row: usize| {
        u16::try_from(row)
            .unwrap_or(u16::MAX)
            .saturating_mul(TOPOLOGY_ROW_STRIDE)
            .saturating_add(TOPOLOGY_ROW_START)
    };
    for (row, node) in nodes.iter_mut().enumerate() {
        let (x, width) = match node.column {
            TopologyColumn::Local => (8, 404),
            TopologyColumn::Wrapper => (36, 376),
            TopologyColumn::Target => (64, 348),
        };
        node.rect = TopologyRect {
            x,
            y: row_y(row),
            width,
            height: TOPOLOGY_NODE_HEIGHT,
        };
    }

    let rects = nodes
        .iter()
        .map(|node| (node.id.as_str(), node.rect))
        .collect::<BTreeMap<_, _>>();
    for edge in edges {
        let (Some(source), Some(target)) = (
            rects.get(edge.source.as_str()),
            rects.get(edge.target.as_str()),
        ) else {
            continue;
        };
        let source_center_x = source.x.saturating_add(source.width / 2);
        let target_center_x = target.x.saturating_add(target.width / 2);
        if source.y < target.y {
            let start = (source_center_x, source.y.saturating_add(source.height));
            let end = (target_center_x, target.y);
            let middle_y = start.1.saturating_add(end.1.saturating_sub(start.1) / 2);
            edge.route = vec![start, (start.0, middle_y), (end.0, middle_y), end];
            edge.label_position = (
                start.0.min(end.0).saturating_add(8),
                middle_y.saturating_sub(8),
            );
        } else {
            let side_x = TOPOLOGY_CANVAS_WIDTH.saturating_sub(2);
            let start = (
                source.x.saturating_add(source.width),
                source.y.saturating_add(source.height / 2),
            );
            let end = (
                target.x.saturating_add(target.width),
                target.y.saturating_add(target.height / 2),
            );
            edge.route = vec![start, (side_x, start.1), (side_x, end.1), end];
            edge.label_position = (
                side_x.saturating_sub(104),
                start.1.min(end.1).saturating_add(8),
            );
        }
    }

    row_y(nodes.len().max(1))
}

fn selected_teaching_operation<'a>(
    problem: Option<&OwnershipProblem>,
    model: &'a OwnershipModel,
) -> Option<&'a rust_analyzer_ext::OwnershipOperationInsight> {
    if let Some(problem) = problem {
        if matches!(
            problem.category.as_str(),
            "multiple_mutable_borrows"
                | "mutable_while_shared"
                | "assign_while_borrowed"
                | "use_while_mutably_borrowed"
                | "immutable_mutation"
        ) && let Some(operation) = selected_mutation_operation(model)
        {
            return Some(operation);
        }
        return model
            .operations
            .iter()
            .filter(|operation| operation.ownership_relevant)
            .min_by_key(|operation| {
                (
                    operation
                        .range
                        .start
                        .line
                        .abs_diff(problem.primary_range.start.line),
                    operation
                        .range
                        .start
                        .character
                        .abs_diff(problem.primary_range.start.character),
                )
            })
            .or_else(|| model.operations.first());
    }
    model
        .operations
        .iter()
        .find(|operation| operation.ownership_relevant)
        .or_else(|| model.operations.first())
}

fn teaching_focus(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
) -> Option<(String, String, Option<lsp::Range>)> {
    let operation = selected_teaching_operation(problem, model);
    let conflict_referent = model.conflict_graph.as_ref().and_then(|graph| {
        graph
            .nodes
            .iter()
            .find(|node| node.role == "borrowed_value")
    });
    let place = operation
        .and_then(|operation| operation.receiver_flow.as_ref())
        .map(|receiver| receiver.expression.clone())
        .or_else(|| conflict_referent.map(|node| node.label.clone()))
        .or_else(|| {
            model
                .mutation_requirement
                .as_ref()
                .map(|requirement| requirement.target_place.clone())
        })
        .or_else(|| model.selected_place.clone())
        .or_else(|| problem.map(|problem| problem.binding_name.clone()))?;
    let root = place_root(&place);
    let type_name = operation
        .and_then(|operation| operation.receiver_type.clone())
        .or_else(|| conflict_referent.and_then(|node| node.type_name.clone()))
        .or_else(|| {
            model
                .bindings
                .iter()
                .find(|binding| binding.name == place || binding.name == root)
                .map(|binding| binding.type_name.clone())
        })
        .or_else(|| {
            model
                .source_context
                .as_ref()
                .and_then(|context| context.related_types.first().cloned())
        })
        .unwrap_or_else(|| "value of unresolved type".to_owned());
    let range = operation
        .and_then(|operation| {
            operation
                .receiver_flow
                .as_ref()
                .map(|receiver| receiver.range)
        })
        .or_else(|| conflict_referent.and_then(|node| node.range))
        .or_else(|| problem.map(|problem| problem.binding_range));
    Some((place, type_name, range))
}

fn teaching_moments(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
    family: TeachingDiagramFamily,
    focus: &str,
    fallback_range: Option<lsp::Range>,
) -> Vec<TopologyMoment> {
    if let Some(graph) = model.conflict_graph.as_ref()
        && !graph.snapshots.is_empty()
    {
        return graph
            .snapshots
            .iter()
            .take(6)
            .map(|snapshot| TopologyMoment {
                title: snapshot.title.clone(),
                explanation: snapshot.explanation.clone(),
                range: snapshot.range,
                path_marker: Some(snapshot.phase.clone()),
            })
            .collect();
    }

    let range = fallback_range
        .or_else(|| problem.map(|problem| problem.primary_range))
        .unwrap_or_default();
    if family == TeachingDiagramFamily::Async {
        return [
            (
                "Created",
                "Calling async code creates a future value without running its body to completion.",
                "before",
            ),
            (
                "Suspended at .await",
                "Polling reached .await. Locals needed later remain stored inside the future while it is pending.",
                "operation",
            ),
            (
                "Resumed",
                "After wake-up, polling resumes from the saved state until the future becomes ready.",
                "after",
            ),
        ]
        .into_iter()
        .map(|(title, explanation, phase)| TopologyMoment {
            title: title.to_owned(),
            explanation: explanation.to_owned(),
            range: selected_teaching_operation(problem, model)
                .map_or(range, |operation| operation.range),
            path_marker: Some(phase.to_owned()),
        })
        .collect();
    }
    if family == TeachingDiagramFamily::Closure {
        return [
            (
                "Environment created",
                "Rust creates a closure environment containing each captured value or reference.",
                "before",
            ),
            (
                "Closure called",
                "The capture mode determines whether the call reads, mutates, or consumes the environment.",
                "operation",
            ),
            (
                "After the call",
                "Fn and FnMut environments remain usable; an FnOnce call may consume captured values.",
                "after",
            ),
        ]
        .into_iter()
        .map(|(title, explanation, phase)| TopologyMoment {
            title: title.to_owned(),
            explanation: explanation.to_owned(),
            range: selected_teaching_operation(problem, model)
                .map_or(range, |operation| operation.range),
            path_marker: Some(phase.to_owned()),
        })
        .collect();
    }
    if let Some(operation) = selected_teaching_operation(problem, model) {
        let (titles, explanations): ([&str; 3], [String; 3]) = match family {
            TeachingDiagramFamily::TraitObject => (
                ["Fat pointer available", "Dynamic call", "Result"],
                [
                    "The trait-object handle carries a data pointer and vtable metadata."
                        .to_owned(),
                    format!(
                        "`{}` follows the vtable entry while passing the data pointer as its receiver.",
                        operation.name
                    ),
                    "Ownership of the data is still determined by the outer reference or smart pointer."
                        .to_owned(),
                ],
            ),
            _ => (
                ["Before", "Operation", "Result"],
                [
                    format!("`{focus}` is available through the access path shown below."),
                    if operation.summary.is_empty() {
                        format!("`{}` uses {}.", operation.name, readable_access(&operation.required_access))
                    } else {
                        operation.summary.clone()
                    },
                    operation
                        .receiver_flow
                        .as_ref()
                        .map(|receiver| receiver.after.clone())
                        .unwrap_or_else(|| "The operation's ownership effects are now reflected in the diagram.".to_owned()),
                ],
            ),
        };
        return titles
            .into_iter()
            .zip(explanations)
            .enumerate()
            .map(|(index, (title, explanation))| TopologyMoment {
                title: title.to_owned(),
                explanation,
                range: if index == 0 {
                    fallback_range.unwrap_or(operation.range)
                } else {
                    operation.range
                },
                path_marker: Some(
                    match index {
                        0 => "before",
                        1 => "operation",
                        _ => "result",
                    }
                    .to_owned(),
                ),
            })
            .collect();
    }

    vec![TopologyMoment {
        title: "Current shape".to_owned(),
        explanation: format!("The selected type determines how `{focus}` reaches its value."),
        range,
        path_marker: Some("shape".to_owned()),
    }]
}

fn selected_phase_kind(moments: &[TopologyMoment], selected_step: usize) -> &'static str {
    let Some(moment) = moments.get(selected_step.min(moments.len().saturating_sub(1))) else {
        return "current";
    };
    let text = format!("{} {}", moment.title, moment.explanation).to_ascii_lowercase();
    if text.contains("reject") || text.contains("conflict") || text.contains("cannot borrow") {
        "conflict"
    } else if text.contains("end")
        || text.contains("after")
        || text.contains("resume")
        || text.contains("result")
    {
        "after"
    } else if text.contains("suspend") || text.contains("operation") || text.contains("call") {
        "operation"
    } else {
        "before"
    }
}

fn teaching_scene_summary(
    family: TeachingDiagramFamily,
    problem: Option<&OwnershipProblem>,
    operation: Option<&rust_analyzer_ext::OwnershipOperationInsight>,
    type_name: &str,
) -> String {
    if family == TeachingDiagramFamily::Sequence
        && operation.is_some_and(|operation| operation.name == "push")
        && problem.is_some_and(|problem| problem.diagnostic_code.as_deref() == Some("E0502"))
    {
        return "A shared reference points into the Vec element buffer. `push` needs exclusive access and may replace that buffer, so Rust keeps the reference valid until its final use and rejects the mutation.".to_owned();
    }
    if let Some(operation) = operation {
        let effect = operation
            .effect_facts
            .first()
            .map(|effect| format!(" {}", effect.summary))
            .unwrap_or_default();
        return format!(
            "`{}` operates on `{type_name}` through {}.{effect}",
            operation.name,
            readable_access(&operation.required_access)
        );
    }
    match family {
        TeachingDiagramFamily::Async => {
            "An async body is stored as a future state machine; values needed after `.await` remain inside that future while it is suspended.".to_owned()
        }
        TeachingDiagramFamily::Closure => {
            "A closure is a compiler-generated value whose fields are the values or references it captures.".to_owned()
        }
        TeachingDiagramFamily::TraitObject => {
            "A trait-object handle combines a data pointer with vtable metadata; its outer wrapper still decides ownership.".to_owned()
        }
        _ => format!("The selected `{type_name}` is expanded from its local handle to the value or storage it reaches."),
    }
}

fn derive_teaching_topology_scene(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
    selected_step: usize,
) -> Option<OwnershipTopologyScene> {
    let (focus, mut type_name, focus_range) = teaching_focus(problem, model)?;
    let operation = selected_teaching_operation(problem, model);
    if problem.is_none() && operation.is_none() && !model.memory_graph.nodes.is_empty() {
        return None;
    }
    let family = teaching_diagram_family(problem, &type_name);
    if type_name == "value of unresolved type" {
        type_name = match family {
            TeachingDiagramFamily::Async => "generated Future state machine".to_owned(),
            TeachingDiagramFamily::Closure => "compiler-generated closure environment".to_owned(),
            _ => type_name,
        };
    }
    let moments = teaching_moments(problem, model, family, &focus, focus_range);
    let selected_step = selected_step.min(moments.len().saturating_sub(1));
    let phase = selected_phase_kind(&moments, selected_step);
    let mut nodes = vec![TopologyNode {
        id: "teaching:focus".to_owned(),
        place: focus.clone(),
        label: focus.clone(),
        type_name: type_name.clone(),
        detail: "selected value or access path".to_owned(),
        kind: "projected_place".to_owned(),
        storage: "inline".to_owned(),
        state: if phase == "conflict" {
            "alive; requested operation blocked".to_owned()
        } else {
            "available".to_owned()
        },
        provenance: "compiler_type".to_owned(),
        column: TopologyColumn::Local,
        range: focus_range,
        rect: TopologyRect::default(),
    }];
    let mut layers = Vec::new();
    append_type_layers(&type_name, "focus", "type", &mut layers, 0);
    if family == TeachingDiagramFamily::Async
        && !layers.iter().any(|layer| layer.kind == "future_state")
    {
        let parent = layers
            .iter()
            .rev()
            .find(|layer| matches!(layer.kind.as_str(), "heap_allocation" | "control_block"))
            .map_or_else(|| "focus".to_owned(), |layer| layer.id_suffix.clone());
        append_type_layers("impl Future", &parent, "async", &mut layers, 1);
    }
    if family == TeachingDiagramFamily::Closure
        && !layers
            .iter()
            .any(|layer| layer.kind == "closure_environment")
    {
        append_type_layers(
            "compiler-generated closure environment",
            "focus",
            "closure",
            &mut layers,
            1,
        );
    }
    let mut edges = Vec::new();
    for layer in layers {
        let id = format!("teaching:{}", layer.id_suffix);
        let parent = format!(
            "teaching:{}",
            layer.parent_suffix.as_deref().unwrap_or("focus")
        );
        let state = if layer.kind == "future_state" {
            match phase {
                "operation" => "polling or suspended as Poll::Pending".to_owned(),
                "after" => "resumed; may become Poll::Ready".to_owned(),
                _ => "created; not yet run to completion".to_owned(),
            }
        } else if layer.kind == "suspension_point" {
            if phase == "operation" {
                "active suspension point".to_owned()
            } else {
                "not currently suspended here".to_owned()
            }
        } else if layer.kind == "closure_environment" {
            match phase {
                "operation" => "closure call is using its captures".to_owned(),
                "after" => "availability depends on Fn, FnMut, or FnOnce".to_owned(),
                _ => "capture environment created".to_owned(),
            }
        } else if phase == "conflict"
            && matches!(layer.kind.as_str(), "buffer" | "element" | "borrowed_view")
        {
            "shared borrow live; mutation blocked".to_owned()
        } else if phase == "after" {
            "available after the operation or final use".to_owned()
        } else {
            "available".to_owned()
        };
        nodes.push(TopologyNode {
            id: id.clone(),
            place: focus.clone(),
            label: layer.label,
            type_name: layer.type_name,
            detail: layer.detail,
            kind: layer.kind.clone(),
            storage: layer.storage.clone(),
            state,
            provenance: layer.provenance.clone(),
            column: topology_column(&layer.kind, &layer.storage),
            range: focus_range,
            rect: TopologyRect::default(),
        });
        edges.push(TopologyEdge {
            id: format!("teaching-edge:{parent}:{id}"),
            source: parent,
            target: id,
            label: layer.relation,
            provenance: layer.provenance,
            active: true,
            range: focus_range,
            route: Vec::new(),
            label_position: (0, 0),
        });
    }

    let element_target = nodes
        .iter()
        .find(|node| node.kind == "element")
        .or_else(|| nodes.iter().find(|node| node.kind == "buffer"))
        .map(|node| node.id.clone())
        .unwrap_or_else(|| "teaching:focus".to_owned());
    if let Some(graph) = model.conflict_graph.as_ref() {
        let snapshot = graph
            .snapshots
            .get(selected_step)
            .or_else(|| graph.snapshots.last());
        let mut role_ids = BTreeMap::new();
        for graph_node in &graph.nodes {
            if graph_node.role == "borrowed_value" {
                role_ids.insert(graph_node.id.as_str(), element_target.clone());
                continue;
            }
            let id = format!("teaching:conflict:{}", graph_node.id);
            let state = snapshot
                .and_then(|snapshot| {
                    snapshot
                        .states
                        .iter()
                        .find(|state| state.node_id == graph_node.id)
                })
                .map(|state| state.state.clone())
                .unwrap_or_else(|| {
                    if graph_node.role == "borrower_reference" && phase != "after" {
                        "shared borrow live".to_owned()
                    } else {
                        "available".to_owned()
                    }
                });
            nodes.push(TopologyNode {
                id: id.clone(),
                place: graph_node.label.clone(),
                label: graph_node.label.clone(),
                type_name: graph_node
                    .type_name
                    .clone()
                    .unwrap_or_else(|| "compiler-resolved role".to_owned()),
                detail: graph_node.memory.clone(),
                kind: if graph_node.role == "borrower_reference" {
                    "reference_binding".to_owned()
                } else {
                    "binding".to_owned()
                },
                storage: "stack".to_owned(),
                state,
                provenance: graph.provenance.clone(),
                column: TopologyColumn::Local,
                range: graph_node.range,
                rect: TopologyRect::default(),
            });
            role_ids.insert(graph_node.id.as_str(), id);
        }
        for graph_edge in &graph.edges {
            let Some(source) = role_ids.get(graph_edge.from.as_str()) else {
                continue;
            };
            let Some(target) = role_ids.get(graph_edge.to.as_str()) else {
                continue;
            };
            edges.push(TopologyEdge {
                id: format!("teaching-conflict:{}:{}", graph_edge.from, graph_edge.to),
                source: source.clone(),
                target: target.clone(),
                label: graph_edge.kind.clone(),
                provenance: graph_edge.provenance.clone(),
                active: phase != "after",
                range: None,
                route: Vec::new(),
                label_position: (0, 0),
            });
        }
    }

    if let Some(requirement) = model.mutation_requirement.as_ref()
        && !nodes
            .iter()
            .any(|node| node.label == requirement.access_source)
    {
        let access_source_id = "teaching:access-source".to_owned();
        nodes.push(TopologyNode {
            id: access_source_id.clone(),
            place: requirement.access_source.clone(),
            label: requirement.access_source.clone(),
            type_name: readable_available_access(&requirement.available_access).to_owned(),
            detail: requirement.explanation.clone(),
            kind: "reference_binding".to_owned(),
            storage: "stack".to_owned(),
            state: format!(
                "provides {}; cannot grant {}",
                readable_available_access(&requirement.available_access),
                readable_access(&requirement.required_access)
            ),
            provenance: requirement.provenance.clone(),
            column: TopologyColumn::Local,
            range: problem.map(|problem| problem.binding_range),
            rect: TopologyRect::default(),
        });
        edges.push(TopologyEdge {
            id: "teaching-edge:access-source".to_owned(),
            source: access_source_id,
            target: "teaching:focus".to_owned(),
            label: format!(
                "{} cannot satisfy {}",
                readable_available_access(&requirement.available_access),
                readable_access(&requirement.required_access)
            ),
            provenance: requirement.provenance.clone(),
            active: phase != "after",
            range: problem.map(|problem| problem.primary_range),
            route: Vec::new(),
            label_position: (0, 0),
        });
    }

    if let Some(operation) = operation {
        let operation_id = "teaching:operation".to_owned();
        let effect = operation
            .effect_facts
            .first()
            .map(|effect| effect.summary.clone())
            .or_else(|| operation.effects.first().cloned())
            .unwrap_or_else(|| operation.why_required.clone());
        nodes.push(TopologyNode {
            id: operation_id.clone(),
            place: operation.name.clone(),
            label: format!(
                "{}(...) needs {}",
                operation.name,
                readable_access(&operation.required_access)
            ),
            type_name: operation.signature.clone(),
            detail: effect,
            kind: "operation".to_owned(),
            storage: "conceptual".to_owned(),
            state: match phase {
                "conflict" => "operation rejected".to_owned(),
                "after" => "operation finished or available afterward".to_owned(),
                _ => "requested operation".to_owned(),
            },
            provenance: operation.provenance.clone(),
            column: TopologyColumn::Local,
            range: Some(operation.range),
            rect: TopologyRect::default(),
        });
        let receiver_target = nodes
            .iter()
            .find(|node| node.kind == "wrapper" && node.type_name.starts_with("Vec<"))
            .map(|node| node.id.clone())
            .unwrap_or_else(|| "teaching:focus".to_owned());
        edges.push(TopologyEdge {
            id: "teaching-edge:operation:receiver".to_owned(),
            source: operation_id,
            target: receiver_target.clone(),
            label: format!("needs_{}", operation.required_access),
            provenance: operation.provenance.clone(),
            active: phase == "operation" || phase == "conflict",
            range: Some(operation.range),
            route: Vec::new(),
            label_position: (0, 0),
        });

        let may_reallocate = operation.effect_facts.iter().any(|effect| {
            effect.kind == "allocation" && effect.summary.to_ascii_lowercase().contains("realloc")
        }) || operation
            .effects
            .iter()
            .any(|effect| effect.to_ascii_lowercase().contains("realloc"));
        if may_reallocate {
            let new_buffer_id = "teaching:possible-new-buffer".to_owned();
            let element_type = parse_type(&type_name)
                .arguments
                .first()
                .cloned()
                .unwrap_or_else(|| "T".to_owned());
            nodes.push(TopologyNode {
                id: new_buffer_id.clone(),
                place: focus.clone(),
                label: format!("Heap B · [{element_type} 0] […] [new {element_type}]"),
                type_name: format!("possible replacement [{element_type}] buffer"),
                detail: "created only if runtime capacity is insufficient; the rejected program never performs this transition".to_owned(),
                kind: "buffer".to_owned(),
                storage: "heap".to_owned(),
                state: if phase == "conflict" {
                    "hypothetical allocation if the rejected push were allowed".to_owned()
                } else {
                    "possible runtime effect, not current storage".to_owned()
                },
                provenance: "conceptual_from_trusted_operation_effect".to_owned(),
                column: TopologyColumn::Target,
                range: Some(operation.range),
                rect: TopologyRect::default(),
            });
            edges.push(TopologyEdge {
                id: "teaching-edge:possible-reallocation".to_owned(),
                source: receiver_target,
                target: new_buffer_id,
                label: "may_reallocate_to".to_owned(),
                provenance: "conceptual_from_trusted_operation_effect".to_owned(),
                active: phase == "conflict" || phase == "operation",
                range: Some(operation.range),
                route: Vec::new(),
                label_position: (0, 0),
            });
        }
    }

    nodes.sort_by(|left, right| {
        let priority = |node: &TopologyNode| match node.kind.as_str() {
            "binding" if node.id.ends_with(":owner") => 0,
            "projected_place" => 1,
            "handle" | "wrapper" => 2,
            "borrow_flag" | "lock_state" | "gate" | "metadata" => 3,
            "heap_allocation" | "control_block" | "buffer"
                if !node.provenance.contains("conceptual") =>
            {
                4
            }
            "element" | "inline_value" | "borrowed_view" => 5,
            "reference_binding" | "binding" => 6,
            "operation" => 7,
            _ if node.provenance.contains("conceptual") => 8,
            _ => 9,
        };
        priority(left)
            .cmp(&priority(right))
            .then_with(|| left.id.cmp(&right.id))
    });
    nodes.dedup_by(|left, right| left.id == right.id);
    edges.sort_by(|left, right| left.id.cmp(&right.id));
    edges.dedup_by(|left, right| left.id == right.id);
    let retained_ids = semantic_node_order(&nodes, &edges, &[focus.as_str()], 12)
        .into_iter()
        .collect::<BTreeSet<_>>();
    nodes.retain(|node| retained_ids.contains(&node.id));
    edges.retain(|edge| retained_ids.contains(&edge.source) && retained_ids.contains(&edge.target));
    edges.truncate(20);
    let canvas_height = layout_topology_scene(&mut nodes, &mut edges);
    let mut access_lines = vec![format!("Selected type: `{type_name}`")];
    if let Some(operation) = operation {
        access_lines.push(format!(
            "Call contract: `{}` requires {}",
            operation.name,
            readable_access(&operation.required_access)
        ));
    }

    Some(OwnershipTopologyScene {
        title: teaching_title(family, &focus),
        summary: teaching_scene_summary(family, problem, operation, &type_name),
        nodes,
        edges,
        moments,
        selected_step,
        access_lines,
        canvas_height,
        expanded: false,
        truncated: false,
        legacy_limited: false,
    })
}

pub(super) fn derive_ownership_topology_scene(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
    selected_step: usize,
) -> Option<OwnershipTopologyScene> {
    derive_teaching_topology_scene(problem, model, selected_step).or_else(|| {
        derive_ownership_topology_scene_with_limits(problem, model, selected_step, 12, 20, false)
    })
}

pub(super) fn derive_ownership_topology_scene_with_limits(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
    selected_step: usize,
    node_limit: usize,
    edge_limit: usize,
    expanded: bool,
) -> Option<OwnershipTopologyScene> {
    if model.memory_graph.nodes.is_empty()
        && model
            .conflict_graph
            .as_ref()
            .is_none_or(|graph| graph.nodes.is_empty())
        && model.mutation_requirement.is_none()
    {
        return None;
    }

    let selected_step = selected_step.min(
        model
            .memory_graph
            .snapshots
            .len()
            .max(
                model
                    .conflict_graph
                    .as_ref()
                    .map_or(0, |graph| graph.snapshots.len()),
            )
            .saturating_sub(1),
    );
    let states = topology_state_at_step(model, selected_step);
    let mut nodes = model
        .memory_graph
        .nodes
        .iter()
        .map(|node| TopologyNode {
            id: node.id.clone(),
            place: node.place.clone(),
            label: node.label.clone(),
            type_name: node.type_name.clone(),
            detail: topology_detail(node),
            kind: node.kind.clone(),
            storage: node.storage.clone(),
            state: states
                .get(&node.id)
                .cloned()
                .unwrap_or_else(|| node.state.clone()),
            provenance: node.provenance.clone(),
            column: topology_column(&node.kind, &node.storage),
            range: node.range,
            rect: TopologyRect::default(),
        })
        .collect::<Vec<_>>();
    let mut edges = model
        .memory_graph
        .edges
        .iter()
        .map(|edge| TopologyEdge {
            id: edge.id.clone(),
            source: edge.source.clone(),
            target: edge.target.clone(),
            label: edge.relation.clone(),
            provenance: edge.provenance.clone(),
            active: topology_edge_active_at_step(edge, model, selected_step),
            range: edge.range,
            route: Vec::new(),
            label_position: (0, 0),
        })
        .collect::<Vec<_>>();

    if nodes.is_empty()
        && let Some(graph) = &model.conflict_graph
    {
        let snapshot = graph
            .snapshots
            .get(selected_step)
            .or_else(|| graph.snapshots.last());
        nodes.extend(graph.nodes.iter().map(|node| {
            let state = snapshot
                .and_then(|snapshot| {
                    snapshot
                        .states
                        .iter()
                        .find(|state| state.node_id == node.id)
                })
                .map(|state| state.state.clone())
                .unwrap_or_else(|| "alive".to_owned());
            let storage = node.memory.to_lowercase();
            TopologyNode {
                id: node.id.clone(),
                place: node.label.clone(),
                label: node.label.clone(),
                type_name: node
                    .type_name
                    .clone()
                    .unwrap_or_else(|| "type unknown".to_owned()),
                detail: node.memory.clone(),
                kind: node.role.clone(),
                storage: storage.clone(),
                state,
                provenance: graph.provenance.clone(),
                column: topology_column(&node.role, &storage),
                range: node.range,
                rect: TopologyRect::default(),
            }
        }));
        edges.extend(graph.edges.iter().map(|edge| TopologyEdge {
            id: format!("conflict:{}:{}:{}", edge.from, edge.to, edge.label),
            source: edge.from.clone(),
            target: edge.to.clone(),
            label: edge.label.clone(),
            provenance: edge.provenance.clone(),
            active: true,
            range: None,
            route: Vec::new(),
            label_position: (0, 0),
        }));
    }

    if nodes.is_empty()
        && let Some(requirement) = &model.mutation_requirement
    {
        let access_id = format!("access:{}", requirement.access_source);
        let target_id = format!("target:{}", requirement.target_place);
        nodes.push(TopologyNode {
            id: access_id.clone(),
            place: requirement.access_source.clone(),
            label: requirement.access_source.clone(),
            type_name: readable_available_access(&requirement.available_access).to_owned(),
            detail: "access available at the function boundary".to_owned(),
            kind: "handle".to_owned(),
            storage: "stack".to_owned(),
            state: "read-only access".to_owned(),
            provenance: requirement.provenance.clone(),
            column: TopologyColumn::Local,
            range: problem.map(|problem| problem.binding_range),
            rect: TopologyRect::default(),
        });
        nodes.push(TopologyNode {
            id: target_id.clone(),
            place: requirement.target_place.clone(),
            label: requirement.target_place.clone(),
            type_name: selected_mutation_operation(model)
                .and_then(|operation| operation.receiver_type.clone())
                .unwrap_or_else(|| "resolved field type".to_owned()),
            detail: "the value rustc rejected writing through".to_owned(),
            kind: "projected_place".to_owned(),
            storage: "inline".to_owned(),
            state: "alive · write blocked".to_owned(),
            provenance: requirement.provenance.clone(),
            column: TopologyColumn::Wrapper,
            range: problem.map(|problem| problem.primary_range),
            rect: TopologyRect::default(),
        });
        edges.push(TopologyEdge {
            id: format!("mutation-access:{}", requirement.target_place),
            source: access_id,
            target: target_id,
            label: format!(
                "has {}; needs {}",
                readable_available_access(&requirement.available_access),
                readable_access(&requirement.required_access)
            ),
            provenance: requirement.provenance.clone(),
            active: true,
            range: problem.map(|problem| problem.primary_range),
            route: Vec::new(),
            label_position: (0, 0),
        });
    }

    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    nodes.dedup_by(|left, right| left.id == right.id);
    edges.sort_by(|left, right| left.id.cmp(&right.id));
    edges.dedup_by(|left, right| left.id == right.id);

    let selected_names = problem
        .map(|problem| problem.binding_name.as_str())
        .into_iter()
        .chain(model.selected_place.as_deref())
        .collect::<Vec<_>>();
    let ordered_ids = semantic_node_order(&nodes, &edges, &selected_names, node_limit);
    let retained_ids = ordered_ids.iter().cloned().collect::<BTreeSet<String>>();
    let original_node_count = nodes.len();
    let original_edge_count = edges.len();
    let nodes_by_id = nodes
        .into_iter()
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let mut nodes = ordered_ids
        .into_iter()
        .filter_map(|id| nodes_by_id.get(&id).cloned())
        .collect::<Vec<_>>();
    edges.retain(|edge| {
        retained_ids.contains(edge.source.as_str()) && retained_ids.contains(edge.target.as_str())
    });
    edges.sort_by(|left, right| {
        relation_priority(&left.label)
            .cmp(&relation_priority(&right.label))
            .then_with(|| left.id.cmp(&right.id))
    });
    edges.truncate(edge_limit);
    let truncated = model.memory_graph.truncated
        || original_node_count > nodes.len()
        || original_edge_count > edges.len();
    let canvas_height = layout_topology_scene(&mut nodes, &mut edges);

    let moments = if !model.memory_graph.snapshots.is_empty() {
        model
            .memory_graph
            .snapshots
            .iter()
            .take(12)
            .map(|snapshot| TopologyMoment {
                title: snapshot.kind.replace('_', " "),
                explanation: topology_moment_explanation(&snapshot.kind, &snapshot.place),
                range: snapshot.range,
                path_marker: snapshot.path_marker.clone(),
            })
            .collect()
    } else {
        model
            .conflict_graph
            .as_ref()
            .map(|graph| {
                graph
                    .snapshots
                    .iter()
                    .take(12)
                    .map(|snapshot| TopologyMoment {
                        title: snapshot.title.clone(),
                        explanation: snapshot.explanation.clone(),
                        range: snapshot.range,
                        path_marker: None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let access_lines = model
        .memory_graph
        .access_paths
        .iter()
        .take(3)
        .map(|path| {
            let chain = path
                .steps
                .iter()
                .map(|step| {
                    format!(
                        "{} → {} ({})",
                        step.starting_type,
                        step.result_type,
                        step.kind.replace('_', " ")
                    )
                })
                .collect::<Vec<_>>()
                .join(" → ");
            if chain.is_empty() {
                format!("`{}`: direct access", path.place)
            } else {
                format!("`{}`: {chain}", path.place)
            }
        })
        .collect();

    Some(OwnershipTopologyScene {
        title: if expanded {
            "Full compiler ownership graph".to_owned()
        } else {
            "Where the selected value lives".to_owned()
        },
        summary: "Compiler and source facts are arranged from local access paths through inline wrappers to heap or borrowed targets.".to_owned(),
        nodes,
        edges,
        moments,
        selected_step,
        access_lines,
        canvas_height,
        expanded,
        truncated,
        legacy_limited: model.compiler_schema_version > 0 && model.compiler_schema_version < 7,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, kind: &str, storage: &str, label: &str, type_name: &str) -> TopologyNode {
        TopologyNode {
            id: id.to_owned(),
            place: "value".to_owned(),
            label: label.to_owned(),
            type_name: type_name.to_owned(),
            detail: storage.to_owned(),
            kind: kind.to_owned(),
            storage: storage.to_owned(),
            state: "available".to_owned(),
            provenance: "compiler_exact".to_owned(),
            column: topology_column(kind, storage),
            range: None,
            rect: TopologyRect::default(),
        }
    }

    fn edge(id: &str, source: &str, target: &str, label: &str) -> TopologyEdge {
        TopologyEdge {
            id: id.to_owned(),
            source: source.to_owned(),
            target: target.to_owned(),
            label: label.to_owned(),
            provenance: "compiler_exact".to_owned(),
            active: true,
            range: None,
            route: Vec::new(),
            label_position: (0, 0),
        }
    }

    fn scene(nodes: Vec<TopologyNode>, edges: Vec<TopologyEdge>) -> OwnershipTopologyScene {
        OwnershipTopologyScene {
            nodes,
            edges,
            ..OwnershipTopologyScene::default()
        }
    }

    #[test]
    fn type_parser_preserves_nested_wrapper_arguments() {
        let parsed = parse_type("std::rc::Rc<RefCell<Vec<Result<Sku, Error>>>>");
        assert_eq!(parsed.outer, "Rc");
        assert_eq!(parsed.arguments, ["RefCell<Vec<Result<Sku, Error>>>"]);

        let result = parse_type("Result<Vec<Sku>, Arc<Error>>");
        assert_eq!(result.arguments, ["Vec<Sku>", "Arc<Error>"]);
    }

    #[test]
    fn nested_wrappers_expand_to_real_storage_layers() {
        let mut layers = Vec::new();
        append_type_layers("Rc<RefCell<Vec<i32>>>", "focus", "type", &mut layers, 0);
        let labels = layers
            .iter()
            .map(|layer| layer.label.as_str())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"Rc handle (shared owner)"));
        assert!(labels.contains(&"Rc allocation [strong | weak | value]"));
        assert!(labels.contains(&"RefCell runtime borrow flag"));
        assert!(labels.contains(&"Vec handle [ptr | len | cap]"));
        assert!(labels.iter().any(|label| label.starts_with("Heap A")));
    }

    #[test]
    fn trait_objects_branch_data_from_vtable_metadata() {
        let mut layers = Vec::new();
        append_type_layers("&dyn Display", "focus", "type", &mut layers, 0);
        let reference = layers
            .iter()
            .find(|layer| layer.id_suffix.contains("reference"))
            .unwrap();
        let data = layers
            .iter()
            .find(|layer| layer.kind == "trait_object_data")
            .unwrap();
        let vtable = layers
            .iter()
            .find(|layer| layer.kind == "metadata")
            .unwrap();
        assert_eq!(
            data.parent_suffix.as_deref(),
            Some(reference.id_suffix.as_str())
        );
        assert_eq!(
            vtable.parent_suffix.as_deref(),
            Some(reference.id_suffix.as_str())
        );
        assert_eq!(vtable.relation, "dispatches_via");
    }

    #[test]
    fn owning_trait_object_keeps_vtable_with_the_fat_handle() {
        let mut layers = Vec::new();
        append_type_layers("Box<dyn Display>", "focus", "type", &mut layers, 0);
        let handle = layers
            .iter()
            .find(|layer| layer.label.contains("fat owner"))
            .unwrap();
        let allocation = layers
            .iter()
            .find(|layer| layer.kind == "heap_allocation")
            .unwrap();
        let vtable = layers
            .iter()
            .find(|layer| layer.kind == "metadata")
            .unwrap();
        assert_eq!(
            allocation.parent_suffix.as_deref(),
            Some(handle.id_suffix.as_str())
        );
        assert_eq!(
            vtable.parent_suffix.as_deref(),
            Some(handle.id_suffix.as_str())
        );
        assert_ne!(vtable.parent_suffix, Some(allocation.id_suffix.clone()));
    }

    #[test]
    fn common_type_families_never_collapse_to_an_empty_picture() {
        for type_name in [
            "String",
            "VecDeque<Sku>",
            "Arc<Mutex<Vec<Sku>>>",
            "Weak<String>",
            "Pin<Box<dyn Future<Output = Sku>>>",
            "Cow<'a, str>",
            "Option<Result<Sku, Error>>",
            "*const Sku",
            "&[Sku]",
            "compiler-generated closure environment",
        ] {
            let mut layers = Vec::new();
            append_type_layers(type_name, "focus", "type", &mut layers, 0);
            assert!(!layers.is_empty(), "missing diagram layers for {type_name}");
        }
    }

    #[test]
    fn async_types_show_saved_locals_and_suspension() {
        let mut layers = Vec::new();
        append_type_layers("impl Future<Output = Sku>", "focus", "type", &mut layers, 0);
        assert!(layers.iter().any(|layer| layer.kind == "future_state"));
        assert!(layers.iter().any(|layer| layer.kind == "capture"));
        assert!(layers.iter().any(|layer| layer.kind == "suspension_point"));
    }

    #[test]
    fn vec_borrow_conflict_builds_the_beginner_reallocation_picture() {
        let model: OwnershipModel = serde_json::from_str(r#"{
            "schemaVersion": 14,
            "compilerSchemaVersion": 7,
            "targetTriple": "x86_64-unknown-linux-gnu",
            "precision": "compiler_exact",
            "status": "ready",
            "truncated": false,
            "sourceHash": "catalog",
            "selectedProblemId": "e0502",
            "selectedPlace": "*self.1",
            "events": [],
            "valueTrace": [],
            "repairs": [],
            "bodies": [],
            "bindings": [],
            "loans": [],
            "memoryGraph": { "nodes": [], "edges": [], "snapshots": [], "accessPaths": [], "truncated": false },
            "operations": [{
                "id": "push",
                "range": { "start": { "line": 53, "character": 8 }, "end": { "line": 53, "character": 41 } },
                "name": "push",
                "signature": "fn push(&mut self, value: Sku)",
                "receiverType": "Vec<Sku>",
                "requiredAccess": "mutable_borrow",
                "availableAccess": "shared_borrow_live",
                "whyRequired": "push can change the vector length and allocation",
                "documentation": null,
                "effects": ["Adds one element and may reallocate."],
                "effectFacts": [{ "kind": "allocation", "summary": "Adds one element and may reallocate.", "certainty": "trusted_standard_library_catalog" }],
                "callChain": ["push"],
                "alternatives": [],
                "provenance": "signature_docs_and_trusted_catalog",
                "truncated": false,
                "summary": "push temporarily needs exclusive access",
                "ownershipRelevant": true,
                "receiverFlow": {
                    "expression": "self.featured",
                    "range": { "start": { "line": 53, "character": 8 }, "end": { "line": 53, "character": 21 } },
                    "transfer": "mutable_borrow",
                    "after": "Usable again when the borrow ends.",
                    "provenance": "resolved_self_parameter"
                },
                "argumentFlows": [],
                "returnFlow": null
            }],
            "mutationRequirement": null,
            "conflictGraph": {
                "title": "shared and mutable borrow conflict",
                "summary": "first still reads from self.featured",
                "requestedAccess": "mutable_borrow",
                "nodes": [
                    { "id": "borrower", "label": "first", "typeName": "&Sku", "role": "borrower_reference", "memory": "stack reference binding", "range": null },
                    { "id": "referent", "label": "self.featured", "typeName": "Vec<Sku>", "role": "borrowed_value", "memory": "live borrowed value", "range": null },
                    { "id": "owner", "label": "self", "typeName": "&mut Catalog", "role": "owner_path", "memory": "owner path", "range": null }
                ],
                "edges": [
                    { "from": "borrower", "to": "referent", "kind": "borrow_shared", "label": "holds a live shared view into", "provenance": "compiler_diagnostic" },
                    { "from": "owner", "to": "referent", "kind": "contains", "label": "keeps alive", "provenance": "source_derived" }
                ],
                "snapshots": [
                    { "phase": "borrow_created", "title": "Borrow created", "explanation": "first points into self.featured", "range": { "start": { "line": 52, "character": 8 }, "end": { "line": 52, "character": 37 } }, "states": [] },
                    { "phase": "operation_rejected", "title": "Operation rejected", "explanation": "cannot borrow self.featured as mutable", "range": { "start": { "line": 53, "character": 8 }, "end": { "line": 53, "character": 41 } }, "states": [] },
                    { "phase": "borrow_ended", "title": "Borrow ended", "explanation": "first has its final use", "range": { "start": { "line": 54, "character": 8 }, "end": { "line": 54, "character": 57 } }, "states": [] }
                ],
                "provenance": "compiler_diagnostic",
                "truncated": false
            },
            "sourceContext": null,
            "cSketch": null
        }"#)
        .unwrap();
        let problem: OwnershipProblem = serde_json::from_value(serde_json::json!({
            "id": "e0502",
            "category": "mutable_while_shared",
            "diagnosticCode": "E0502",
            "message": "cannot borrow self.featured as mutable because it is also borrowed as immutable",
            "bindingName": "self",
            "primaryRange": { "start": { "line": 53, "character": 8 }, "end": { "line": 53, "character": 41 } },
            "bindingRange": { "start": { "line": 52, "character": 8 }, "end": { "line": 52, "character": 37 } },
            "relatedRanges": [],
            "related": [],
            "modelPosition": { "line": 53, "character": 8 },
            "precision": "compiler_exact"
        }))
        .unwrap();

        let scene = derive_teaching_topology_scene(Some(&problem), &model, 1).unwrap();
        let labels = scene
            .nodes
            .iter()
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();
        assert!(scene.title.contains("self.featured"));
        assert!(labels.contains(&"self.featured"));
        assert!(labels.contains(&"first"));
        assert!(labels.contains(&"Vec handle [ptr | len | cap]"));
        assert!(labels.iter().any(|label| label.starts_with("Heap A")));
        assert!(labels.iter().any(|label| label.starts_with("Heap B")));
        assert!(scene.edges.iter().any(|edge| {
            edge.label == "may_reallocate_to" && edge.provenance.contains("conceptual")
        }));
        assert_eq!(scene.moments.len(), 3);
        assert_eq!(scene.selected_step, 1);
    }

    #[test]
    fn beginner_path_keeps_every_nested_wrapper_layer_in_order() {
        let scene = scene(
            vec![
                node(
                    "binding",
                    "binding",
                    "stack",
                    "variable value",
                    "Rc<RefCell<Vec<i32>>>",
                ),
                node(
                    "rc-handle",
                    "handle",
                    "inline",
                    "Rc handle",
                    "Rc<RefCell<Vec<i32>>>",
                ),
                node(
                    "rc-allocation",
                    "control_block",
                    "heap",
                    "Rc allocation",
                    "RefCell<Vec<i32>>",
                ),
                node(
                    "refcell",
                    "borrow_flag",
                    "inline",
                    "RefCell borrow gate",
                    "RefCell<Vec<i32>>",
                ),
                node("vec-header", "wrapper", "inline", "Vec header", "Vec<i32>"),
                node("vec-buffer", "buffer", "heap", "Vec element buffer", "i32"),
            ],
            vec![
                edge("e1", "binding", "rc-handle", "stores"),
                edge("e2", "rc-handle", "rc-allocation", "shares_allocation"),
                edge("e3", "rc-allocation", "refcell", "contains"),
                edge("e4", "refcell", "vec-header", "guards_access"),
                edge("e5", "vec-header", "vec-buffer", "owns_buffer"),
            ],
        );

        let path = derive_beginner_memory_path(&scene);
        assert_eq!(path.stages.len(), 6);
        assert_eq!(
            path.stages
                .iter()
                .map(|stage| beginner_memory_role(&stage.nodes[0]))
                .collect::<Vec<_>>(),
            [
                "Local variable",
                "Inline owner handle",
                "Shared heap allocation",
                "Runtime borrow gate",
                "Inline collection header",
                "Heap element buffer",
            ]
        );
        assert_eq!(
            path.stages
                .iter()
                .skip(1)
                .map(|stage| stage.relation_from_previous[0].as_str())
                .collect::<Vec<_>>(),
            [
                "holds this handle",
                "shares ownership of",
                "contains",
                "controls access to",
                "owns the element storage",
            ]
        );
    }

    #[test]
    fn beginner_path_groups_aliases_before_their_shared_allocation() {
        let scene = scene(
            vec![
                node("a", "binding", "stack", "variable a", "Rc<String>"),
                node("b", "binding", "stack", "variable b", "Rc<String>"),
                node("ha", "handle", "inline", "Rc handle a", "Rc<String>"),
                node("hb", "handle", "inline", "Rc handle b", "Rc<String>"),
                node(
                    "allocation",
                    "control_block",
                    "heap",
                    "shared Rc allocation",
                    "String",
                ),
            ],
            vec![
                edge("e1", "a", "ha", "stores"),
                edge("e2", "b", "hb", "stores"),
                edge("e3", "ha", "allocation", "shares_allocation"),
                edge("e4", "hb", "allocation", "shares_allocation"),
            ],
        );

        let path = derive_beginner_memory_path(&scene);
        assert_eq!(path.stages.len(), 3);
        assert_eq!(path.stages[0].nodes.len(), 2);
        assert_eq!(path.stages[1].nodes.len(), 2);
        assert_eq!(path.stages[2].nodes.len(), 1);
        assert_eq!(
            path.stages[2].relation_from_previous,
            ["shares ownership of"]
        );
    }

    #[test]
    fn extreme_visual_step_is_clamped_without_overflow() {
        let model = OwnershipModel::default();
        assert!(topology_state_at_step(&model, usize::MAX).is_empty());

        let edge = rust_analyzer_ext::OwnershipMemoryEdge {
            id: "edge".to_owned(),
            source: "source".to_owned(),
            target: "target".to_owned(),
            relation: "owns".to_owned(),
            event_id: None,
            loan_id: None,
            range: None,
            provenance: "compiler_exact".to_owned(),
            path_marker: None,
        };
        assert!(topology_edge_active_at_step(&edge, &model, usize::MAX));
    }

    #[test]
    fn oversized_layout_saturates_instead_of_panicking() {
        let mut nodes = (0..1_000)
            .map(|index| node(&format!("node-{index}"), "binding", "stack", "value", "T"))
            .collect::<Vec<_>>();
        let height = layout_topology_scene(&mut nodes, &mut []);

        assert_eq!(height, u16::MAX);
        assert_eq!(nodes.last().map(|node| node.rect.y), Some(u16::MAX));
        assert!(semantic_node_order(&nodes, &[], &[], 0).is_empty());
    }
}
