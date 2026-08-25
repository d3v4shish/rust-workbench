pub(crate) struct ConceptLesson {
    pub id: &'static str,
    pub title: &'static str,
    pub one_line: &'static str,
    pub rule: &'static str,
    pub memory_model: &'static str,
    pub why: &'static str,
    pub misconception: &'static str,
    #[allow(dead_code)]
    pub checkpoint: &'static str,
    #[cfg_attr(not(test), allow(dead_code))]
    pub choices: [&'static str; 2],
    #[cfg_attr(not(test), allow(dead_code))]
    pub correct_choice: usize,
    #[cfg_attr(not(test), allow(dead_code))]
    pub related: &'static [&'static str],
}

pub(crate) struct RepairIdea {
    pub title: &'static str,
    pub intent: &'static str,
    pub tradeoff: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BeginnerVocabulary {
    pub term: &'static str,
    pub meaning: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BeginnerNarrative {
    pub title: String,
    pub before: String,
    pub operation: String,
    pub now: String,
    pub common_expectation: String,
    pub rust_model: String,
    pub intuition: String,
    pub misconception: String,
    pub vocabulary: Vec<BeginnerVocabulary>,
}

#[allow(clippy::too_many_arguments)]
fn narrative(
    title: impl Into<String>,
    before: impl Into<String>,
    operation: impl Into<String>,
    now: impl Into<String>,
    common_expectation: impl Into<String>,
    rust_model: impl Into<String>,
    intuition: impl Into<String>,
    misconception: impl Into<String>,
    vocabulary: &[(&'static str, &'static str)],
) -> BeginnerNarrative {
    BeginnerNarrative {
        title: title.into(),
        before: before.into(),
        operation: operation.into(),
        now: now.into(),
        common_expectation: common_expectation.into(),
        rust_model: rust_model.into(),
        intuition: intuition.into(),
        misconception: misconception.into(),
        vocabulary: vocabulary
            .iter()
            .map(|(term, meaning)| BeginnerVocabulary { term, meaning })
            .collect(),
    }
}

pub(crate) fn beginner_narrative(category: &str, target: &str) -> BeginnerNarrative {
    match category {
        "use_after_move" => narrative(
            format!("`{target}` was handed to a new owner"),
            format!("`{target}` was the one place responsible for this value."),
            "An assignment or function call transferred that responsibility to another place.",
            format!("The value still exists, but `{target}` is no longer a usable way to reach it."),
            "In many managed languages, assigning an object variable leaves both names usable.",
            "For a non-Copy Rust value, assignment normally transfers the owning handle unless sharing or cloning is explicit.",
            "Think of ownership as the responsibility to clean up, not as the physical location of the bytes.",
            "A move usually does not move the heap allocation; it changes which place controls it.",
            &[("owner", "the place responsible for cleanup"), ("move", "transfer that responsibility"), ("Copy", "a type whose value may be duplicated implicitly")],
        ),
        "partial_move" => narrative(
            format!("Part of `{target}` was handed away"),
            format!("`{target}` was a complete value with all of its fields available."),
            "Code moved one non-Copy field out of the value.",
            "Unmoved fields may still be usable, but the whole value is incomplete until that field is replaced.",
            "Many languages keep an object usable after reading one of its fields.",
            "Rust distinguishes reading a field from moving an owned field out of its parent.",
            "Picture a form with one required page removed: the remaining pages exist, but the complete packet cannot be submitted.",
            "A partial move does not necessarily invalidate every sibling field.",
            &[("place", "a variable, field, or other storage path"), ("partial move", "moving one part of a larger value"), ("reinitialize", "put a new value back into a moved place")],
        ),
        "multiple_mutable_borrows" => narrative(
            format!("`{target}` already has an exclusive visitor"),
            "The value was available for an exclusive mutable borrow.",
            "One mutable borrow began, then code tried to create another before the first one's final use.",
            "The value remains alive, but only the existing mutable reference may access it during that overlap.",
            "Other languages often allow several references to mutate the same object and coordinate at runtime.",
            "Rust requires one exclusive mutable access path at a time so aliases cannot observe an in-progress mutation.",
            "An `&mut` is a temporary exclusive-access pass, not merely a pointer that permits writes.",
            "The first borrow may end before the surrounding block ends; its final use is what matters.",
            &[("mutable borrow", "temporary exclusive access"), ("alias", "another path to the same value"), ("overlap", "two access permissions active at the same time")],
        ),
        "mutable_while_shared" | "assign_while_borrowed" => narrative(
            format!("`{target}` is still being observed"),
            "One or more shared references could read the current value.",
            "Code tried to mutate, replace, or exclusively borrow the value while those readers were still live.",
            "Rust keeps the old view stable until the shared references have had their final use.",
            "Managed languages commonly allow an object to change while other references still point at it.",
            "A Rust shared borrow promises that the borrowed value will not be mutated through ordinary access for that loan's duration.",
            "Readers receive a stable-view promise; mutation waits until nobody can still rely on that view.",
            "Shared means read-only access, not shared ownership. `Rc<T>` and `&T` answer different questions.",
            &[("shared borrow", "temporary read-only access"), ("loan", "the compiler-tracked borrow period"), ("final use", "the last point a reference is needed")],
        ),
        "use_while_mutably_borrowed" => narrative(
            format!("`{target}` is temporarily behind an exclusive reference"),
            "The owner could use the value normally.",
            "Code created a mutable reference and then tried to use the original path before that reference's final use.",
            "The original path is paused while exclusive access is active; the value itself is still alive.",
            "A reference in another language is often just another equally valid route to the object.",
            "An active `&mut T` temporarily becomes the only permitted route to that `T`.",
            "Opening an exclusive editing session temporarily disables other views of the same document.",
            "Borrowed does not mean moved or destroyed; it means access is temporarily constrained.",
            &[("exclusive access", "one active read/write route"), ("borrow", "temporary access without taking ownership"), ("owner", "the path that resumes access after the loan")],
        ),
        "move_while_borrowed" => narrative(
            format!("`{target}` cannot leave while a reference points into it"),
            "A reference was created from the value and may still be used.",
            "Code tried to transfer ownership before that reference's final use.",
            "The transfer is blocked because the old owner must stay responsible for valid borrowed access.",
            "A garbage-collected object may remain alive automatically while any reference reaches it.",
            "A Rust borrow is tied to a proven owner lifetime; ownership cannot transfer in a way that breaks that proof.",
            "Do not forward a package while someone is still using an item that must be returned to that package.",
            "Moving an owning handle is about control and cleanup, even if the underlying heap address would stay unchanged.",
            &[("referent", "the value a reference points into"), ("lifetime", "the region where that reference may be used"), ("move", "transfer ownership")],
        ),
        "move_out_of_borrowed_content" => narrative(
            format!("A borrow of `{target}` does not own its contents"),
            "Code had temporary access to a value owned somewhere else.",
            "It tried to take an owned, non-Copy part through that borrowed path.",
            "Rust blocks the move because the original owner would be left with a missing part it still must manage.",
            "Reading a field through an object reference often leaves the containing object structurally unchanged.",
            "Moving a Rust field is different from reading it; only an owner may normally remove owned content.",
            "A visitor may inspect or modify permitted contents, but cannot take away a required component of the host object.",
            "Cloning a part and moving the original part have different identity and cost semantics.",
            &[("borrowed content", "data reached through a reference"), ("move out", "remove ownership from its current place"), ("clone", "create a separate owned value explicitly")],
        ),
        "immutable_mutation" => narrative(
            format!("The path to `{target}` does not grant write access"),
            "The value exists and is reachable through the current binding, reference, or wrapper.",
            "Code called an operation that needs exclusive mutable access.",
            "The write is blocked; the value is still alive and unchanged.",
            "Object references in many languages permit mutation unless the object or field is specially marked immutable.",
            "Rust makes write permission part of the binding, reference, method receiver, and wrapper path.",
            "Reaching a value and being allowed to change it are separate capabilities.",
            "Making the outer variable `mut` cannot create mutable access through a wrapper whose API only provides shared access.",
            &[("mutable binding", "a variable that may be reassigned or mutably borrowed"), ("`&mut self`", "a method requires exclusive access to its receiver"), ("interior mutability", "a wrapper that checks mutation by another mechanism")],
        ),
        "missing_lifetime" | "borrowed_value_too_short" => narrative(
            format!("Rust cannot prove how long `{target}` remains valid"),
            "A reference was created from storage owned elsewhere.",
            "The reference was stored, returned, or used across a boundary that needs a lifetime relationship.",
            "Rust rejects the path until it can prove the referenced storage outlives every use.",
            "Garbage collection often keeps an object alive as long as a reference can reach it.",
            "Rust references do not extend storage lifetime; the compiler must connect each reference to an owner that already lives long enough.",
            "A lifetime is an expiry relationship printed on a permission slip, not extra runtime storage.",
            "Writing a lifetime annotation describes a relationship; it cannot make a short-lived owner live longer.",
            &[("lifetime", "how reference validity relates to owner scope"), ("outlive", "remain valid for at least as long"), ("annotation", "a name for a lifetime relationship")],
        ),
        "returning_local_reference" => narrative(
            format!("`{target}` is local to the function that is ending"),
            "The function created and owned a local value.",
            "It tried to return a reference into that local value.",
            "The local owner must be cleaned up at return, so the reference would point into storage that no longer belongs to a live value.",
            "A managed runtime can keep an object alive after its creating function returns.",
            "Rust normally ends local ownership at the function boundary unless the owned value itself is returned or stored elsewhere.",
            "Return the book, not a library card for a library that closes when the function exits.",
            "Heap allocation alone does not make data independent of its local owner.",
            &[("local owner", "a value cleaned up when its scope ends"), ("dangling reference", "a reference whose referent no longer exists"), ("owned return", "transfer the value itself to the caller")],
        ),
        "temporary_dropped_while_borrowed" => narrative(
            format!("The temporary owner behind `{target}` ends too soon"),
            "An expression created a short-lived temporary value.",
            "Code borrowed from that temporary and kept the reference past the temporary's drop point.",
            "The borrow is rejected because its referent would be cleaned up before the reference's final use.",
            "Managed runtimes often extend an anonymous object's life while a reference reaches it.",
            "Rust temporaries have defined drop points; borrowing one does not generally promote it to a longer owner scope.",
            "Give the owner a name in the scope where its borrowed data must remain usable.",
            "The reference is not the owner and therefore cannot keep the temporary alive by itself.",
            &[("temporary", "an unnamed intermediate value"), ("drop point", "where cleanup runs"), ("referent", "the value being borrowed")],
        ),
        "trait_requirement" => narrative(
            format!("`{target}` lacks a capability required here"),
            "An API or generic function stated a trait requirement for every accepted type.",
            "Code supplied a type for which that capability has not been proven.",
            "The operation is unavailable until the representation, bound, or implementation matches the contract.",
            "Interfaces in other languages are sometimes checked mainly at object construction or dynamically at a call.",
            "Rust proves trait capabilities at compile time and propagates generic requirements through every call boundary.",
            "A trait bound is a capability checklist the compiler must complete before admitting the call.",
            "A missing trait does not always mean you should implement it; the chosen representation may be the real mismatch.",
            &[("trait", "a named set of supported behavior"), ("bound", "a required trait on a generic type"), ("implementation", "proof that a type supplies a trait")],
        ),
        "type_mismatch" => narrative(
            format!("The value produced for `{target}` has a different contract"),
            "The surrounding expression expected one specific type.",
            "A producer supplied another type, wrapper, reference, or error shape.",
            "Rust stops at the boundary so conversion, failure, allocation, and ownership changes remain explicit.",
            "Dynamically typed languages may defer this mismatch until the value is used.",
            "Rust checks the producer and consumer contracts before running the program.",
            "Types are not just printed shapes; they also state ownership, optionality, failure, and allowed operations.",
            "Similar displayed values do not imply interchangeable types or a free conversion.",
            &[("expected type", "the consumer's required contract"), ("found type", "the producer's actual contract"), ("conversion", "an explicit transformation between representations")],
        ),
        "method_or_trait_unavailable" => narrative(
            format!("The requested operation is not available through `{target}`"),
            "Method lookup started from the receiver's current type and access path.",
            "Rust searched inherent methods and visible trait methods through allowed borrow and dereference steps.",
            "No compatible method contract was proven for this receiver path.",
            "Some languages dispatch a method name dynamically and fail only if the object cannot answer it at runtime.",
            "Rust resolves one method at compile time and proves its trait scope, receiver layer, and access requirements.",
            "The inner value may know the operation while the wrapper you currently hold does not grant that route.",
            "A method existing on `T` does not imply it is available mutably through `Rc<T>` or `&T`.",
            &[("receiver", "the value before the dot in a method call"), ("trait in scope", "a visible source of extension methods"), ("auto-deref", "compiler-assisted search through dereference layers")],
        ),
        "closure_may_outlive_borrow" | "borrowed_data_escapes" => narrative(
            format!("The callable may keep access to `{target}` too long"),
            "A closure or task captured a value from its surrounding scope.",
            "The callable was returned, stored, spawned, or otherwise allowed to outlive that local borrow.",
            "Rust requires the environment to own what it needs or to stay inside the proven borrow scope.",
            "Managed closures often keep captured objects alive automatically.",
            "A Rust closure has a concrete hidden environment containing either references or owned captures, each with normal lifetime rules.",
            "Treat a closure as a small struct: ask whether each stored field is a borrowed reference or an owned value.",
            "The `move` keyword changes capture ownership; it does not necessarily allocate or copy every captured value.",
            &[("capture", "a surrounding value stored in a closure environment"), ("`move` closure", "a closure that takes ownership of captures"), ("escape", "remain usable beyond the current scope")],
        ),
        "await_outside_async" => narrative(
            format!("`{target}` tries to pause a non-async context"),
            "The current function or block executes synchronously.",
            "Code used `.await`, which may suspend execution and later resume it.",
            "Rust needs an async state-machine boundary to store the values required across that suspension.",
            "Some runtimes let any function block until asynchronous work completes.",
            "Rust separates blocking from awaiting; `.await` is only valid where a future state machine is being built.",
            "An async block is resumable work with saved locals, not just a function that happens to call networking code.",
            "Calling an async function creates a future; it does not run that future to completion immediately.",
            &[("future", "a value representing work that can make progress"), ("`.await`", "suspend this future until another is ready"), ("async context", "a function or block compiled as a future")],
        ),
        "recursive_async_function" => narrative(
            format!("`{target}` would contain itself without a size boundary"),
            "An async function produces a concrete future state machine.",
            "The function called itself recursively, so one future state would need to store another future of the same type.",
            "Rust requires indirection so the recursive future has a finite known size.",
            "Managed runtimes routinely place call frames behind runtime-managed references.",
            "Rust future types are concrete values; direct recursive containment needs a pointer-sized boundary such as boxing.",
            "A box turns 'contains another complete copy of me' into 'contains a fixed-size handle to another copy.'",
            "The recursion problem is the future's representation, not recursion in ordinary synchronous functions.",
            &[("state machine", "the stored states of resumable async work"), ("indirection", "reach a value through a fixed-size handle"), ("boxed future", "a future stored behind heap indirection")],
        ),
        _ => narrative(
            format!("Rust rejected an operation involving `{target}`"),
            "The surrounding code established a type, ownership, or access contract.",
            "This operation required a capability that the current path could not prove.",
            "The program remains unchanged; the compiler is asking you to make the intended contract explicit.",
            "Other languages may defer some ownership, type, or capability checks until runtime.",
            "Rust tries to prove the relevant contract before the operation can run.",
            "Start by asking who owns the value, who may access it now, and how long that access must last.",
            "The compiler message describes the failed proof; it does not decide which program design you intended.",
            &[("owner", "the place responsible for a value"), ("access", "permission to read, write, or consume"), ("contract", "what an API requires and guarantees")],
        ),
    }
}

pub(crate) fn lesson(id: &str) -> Option<&'static ConceptLesson> {
    LESSONS.iter().find(|lesson| lesson.id == id)
}

#[cfg(test)]
pub(crate) fn all_lessons() -> &'static [ConceptLesson] {
    LESSONS
}

pub(crate) fn lesson_ids_for_problem(
    category: &str,
    diagnostic_code: Option<&str>,
) -> Vec<&'static str> {
    let primary = match category {
        "use_after_move" => "moves",
        "partial_move" => "partial_moves",
        "multiple_mutable_borrows" => "exclusive_borrow",
        "mutable_while_shared" | "assign_while_borrowed" => "borrow_conflicts",
        "use_while_mutably_borrowed" => "exclusive_borrow",
        "move_while_borrowed" | "move_out_of_borrowed_content" => "moves_and_borrows",
        "immutable_mutation" => "mutability_contracts",
        "missing_lifetime" | "borrowed_value_too_short" => "lifetimes",
        "returning_local_reference" => "returning_references",
        "temporary_dropped_while_borrowed" => "temporary_lifetimes",
        "trait_requirement" => "trait_bounds",
        "type_mismatch" => "type_mismatches",
        "method_or_trait_unavailable" => "method_resolution",
        "closure_may_outlive_borrow" | "borrowed_data_escapes" => "closure_captures",
        "await_outside_async" | "recursive_async_function" => "async_futures",
        _ => match diagnostic_code {
            Some("E0277") => "trait_bounds",
            Some("E0308") => "type_mismatches",
            _ => "borrow_conflicts",
        },
    };
    let mut ids = vec![primary];
    match category {
        "use_after_move" | "partial_move" => ids.push("ownership"),
        "multiple_mutable_borrows"
        | "mutable_while_shared"
        | "assign_while_borrowed"
        | "use_while_mutably_borrowed" => ids.push("non_lexical_lifetimes"),
        "immutable_mutation" => ids.push("method_receivers"),
        "trait_requirement" => ids.push("send_sync"),
        "closure_may_outlive_borrow" | "borrowed_data_escapes" => ids.push("lifetimes"),
        _ => {}
    }
    ids
}

pub(crate) fn repair_ideas(category: &str) -> &'static [RepairIdea] {
    match category {
        "use_after_move" | "partial_move" => MOVE_REPAIRS,
        "multiple_mutable_borrows"
        | "mutable_while_shared"
        | "assign_while_borrowed"
        | "use_while_mutably_borrowed" => BORROW_REPAIRS,
        "move_while_borrowed" | "move_out_of_borrowed_content" => MOVE_BORROW_REPAIRS,
        "immutable_mutation" => MUTABILITY_REPAIRS,
        "missing_lifetime"
        | "borrowed_value_too_short"
        | "returning_local_reference"
        | "temporary_dropped_while_borrowed" => LIFETIME_REPAIRS,
        "trait_requirement" => TRAIT_REPAIRS,
        "type_mismatch" => TYPE_REPAIRS,
        "method_or_trait_unavailable" => METHOD_REPAIRS,
        "closure_may_outlive_borrow" | "borrowed_data_escapes" => CLOSURE_REPAIRS,
        "await_outside_async" | "recursive_async_function" => ASYNC_REPAIRS,
        _ => &[],
    }
}

const MOVE_REPAIRS: &[RepairIdea] = &[
    RepairIdea {
        title: "Borrow instead of transferring ownership",
        intent: "Use `&T` or `&mut T` when the callee only needs temporary access.",
        tradeoff: "The owner must remain alive and compatible borrows may not overlap.",
    },
    RepairIdea {
        title: "Clone an independent value",
        intent: "Use `clone` when both locations should own separate logical values.",
        tradeoff: "Cloning may allocate or copy; later mutations are independent.",
    },
    RepairIdea {
        title: "Share ownership explicitly",
        intent: "Use `Rc<T>` on one thread or `Arc<T>` across threads when owners share one value.",
        tradeoff: "Reference counts add runtime work and do not by themselves permit mutation.",
    },
];

const BORROW_REPAIRS: &[RepairIdea] = &[
    RepairIdea {
        title: "End the earlier borrow sooner",
        intent: "Move its final use before the mutation or place the borrow in a smaller scope.",
        tradeoff: "Usually zero-cost, but requires reorganizing the data flow.",
    },
    RepairIdea {
        title: "Split disjoint fields or elements",
        intent: "Borrow independent places with APIs such as `split_at_mut` or destructuring.",
        tradeoff: "The separation must be statically provable.",
    },
    RepairIdea {
        title: "Use interior mutability deliberately",
        intent: "Use `RefCell<T>` for single-threaded runtime-checked mutation or a lock across threads.",
        tradeoff: "Borrow conflicts become runtime errors or blocking rather than compile errors.",
    },
];

const MOVE_BORROW_REPAIRS: &[RepairIdea] = &[
    RepairIdea {
        title: "Use the reference before moving",
        intent: "Make the last reference use occur before ownership is transferred.",
        tradeoff: "Preserves ownership semantics without allocation.",
    },
    RepairIdea {
        title: "Return or store owned data",
        intent: "Copy or clone only the part that must outlive the borrow.",
        tradeoff: "Creates an independent value and may allocate.",
    },
];

const MUTABILITY_REPAIRS: &[RepairIdea] = &[
    RepairIdea {
        title: "Provide an exclusive mutable path",
        intent: "Make the binding mutable and use `&mut self` or `&mut T` when mutation is the API contract.",
        tradeoff: "Callers must temporarily grant exclusive access.",
    },
    RepairIdea {
        title: "Return a changed value",
        intent: "Use a functional API that consumes or borrows the old value and returns a new one.",
        tradeoff: "May move or allocate, but makes state changes explicit.",
    },
    RepairIdea {
        title: "Use the right interior-mutability wrapper",
        intent: "Choose `Cell`, `RefCell`, `Mutex`, or `RwLock` only when shared access must mutate state.",
        tradeoff: "Adds runtime checks, synchronization, or type restrictions.",
    },
];

const LIFETIME_REPAIRS: &[RepairIdea] = &[
    RepairIdea {
        title: "Return owned data",
        intent: "Return `String`, `Vec<T>`, or another owner when the result must outlive local storage.",
        tradeoff: "Ownership transfer may allocate but removes a lifetime dependency.",
    },
    RepairIdea {
        title: "Tie the output to an input lifetime",
        intent: "Add a lifetime parameter only when the returned reference points into an input.",
        tradeoff: "The caller cannot keep the result longer than that input.",
    },
    RepairIdea {
        title: "Extend the owner's scope",
        intent: "Bind the temporary or owner outside the region where its reference is used.",
        tradeoff: "Keeps the value alive longer and may retain resources longer.",
    },
];

const TRAIT_REPAIRS: &[RepairIdea] = &[
    RepairIdea {
        title: "Satisfy the required bound",
        intent: "Implement the trait or constrain the generic parameter to a type that does.",
        tradeoff: "Narrows which types callers may supply.",
    },
    RepairIdea {
        title: "Change the representation",
        intent: "Use a type whose `Send`, `Sync`, `Clone`, formatting, or conversion behavior matches the API.",
        tradeoff: "May change sharing, allocation, or thread-safety semantics.",
    },
];

const TYPE_REPAIRS: &[RepairIdea] = &[
    RepairIdea {
        title: "Make producer and consumer agree",
        intent: "Change the expression, parameter, return type, or pattern to the intended type.",
        tradeoff: "Changing a public signature affects every caller.",
    },
    RepairIdea {
        title: "Convert explicitly",
        intent: "Use `From`/`Into`, `TryFrom`/`TryInto`, parsing, borrowing, or dereferencing when conversion is intentional.",
        tradeoff: "Fallible conversions require an error path; allocating conversions have a cost.",
    },
];

const METHOD_REPAIRS: &[RepairIdea] = &[
    RepairIdea {
        title: "Bring the defining trait into scope",
        intent: "Import the trait when the receiver implements it but method lookup cannot see it.",
        tradeoff: "May introduce method-name ambiguity that needs qualification.",
    },
    RepairIdea {
        title: "Use the method on the correct layer",
        intent: "Borrow, dereference, unwrap, or access the inner value only when that matches the ownership design.",
        tradeoff: "Unwrapping may fail; dereferencing shared wrappers may not permit mutation.",
    },
];

const CLOSURE_REPAIRS: &[RepairIdea] = &[
    RepairIdea {
        title: "Move captured values into the closure",
        intent: "Use `move` when the closure or task must own its environment.",
        tradeoff: "The outer scope loses non-Copy captures unless they are cloned first.",
    },
    RepairIdea {
        title: "Keep execution inside the borrow's scope",
        intent: "Run or join the closure before borrowed inputs go out of scope.",
        tradeoff: "Reduces concurrency or changes control flow.",
    },
];

const ASYNC_REPAIRS: &[RepairIdea] = &[
    RepairIdea {
        title: "Move the await into an async context",
        intent: "Make the containing function or block async and await the resulting future from an executor.",
        tradeoff: "Async propagates through call boundaries and changes the return type to a future.",
    },
    RepairIdea {
        title: "Box recursive futures",
        intent: "Introduce indirection when recursive async calls would otherwise create an infinitely sized future.",
        tradeoff: "Heap allocation and dynamic dispatch may be introduced.",
    },
];

const LESSONS: &[ConceptLesson] = &[
    ConceptLesson {
        id: "ownership",
        title: "Ownership",
        one_line: "Every non-Copy value has one place responsible for dropping it.",
        rule: "A value may move to a new owner, be borrowed temporarily, or be cloned into a new value.",
        memory_model: "The owner is a control responsibility, not necessarily the physical address of the bytes.",
        why: "One drop responsibility prevents double free, use-after-free, and unclear cleanup.",
        misconception: "Heap allocation does not imply shared ownership; `Box<T>` still has one owner.",
        checkpoint: "Does assigning a `String` to another variable normally copy its heap buffer?",
        choices: ["No, ownership moves", "Yes, all assignments clone"],
        correct_choice: 0,
        related: &["moves", "borrow_conflicts", "smart_pointers"],
    },
    ConceptLesson {
        id: "moves",
        title: "Moves",
        one_line: "A move transfers drop responsibility and invalidates the old place.",
        rule: "After moving a non-Copy value, use the new owner or reinitialize the old place.",
        memory_model: "The payload often stays where it is; the owning handle and cleanup responsibility transfer.",
        why: "Treating both names as owners could drop the same resource twice.",
        misconception: "A move is not necessarily a byte-for-byte heap copy.",
        checkpoint: "After `let b = a` for a `String`, which name owns the string?",
        choices: ["b", "a and b"],
        correct_choice: 0,
        related: &["ownership", "moves_and_borrows", "smart_pointers"],
    },
    ConceptLesson {
        id: "partial_moves",
        title: "Partial moves",
        one_line: "Moving one non-Copy field can leave unrelated fields usable while the whole struct is incomplete.",
        rule: "Use unmoved fields directly, or reinitialize moved fields before using the complete value again.",
        memory_model: "Availability is tracked per place, including fields and indexed projections when provable.",
        why: "Field-sensitive tracking permits useful code without pretending the complete value is intact.",
        misconception: "A partial move does not automatically destroy or move every field.",
        checkpoint: "Can an unmoved field sometimes be used after another field moves?",
        choices: ["Yes", "No"],
        correct_choice: 0,
        related: &["moves", "reinitialization"],
    },
    ConceptLesson {
        id: "borrow_conflicts",
        title: "Borrow conflicts",
        one_line: "A live shared view blocks mutation; a live exclusive view blocks every competing access.",
        rule: "At a given time, Rust permits either many shared borrows or one mutable borrow.",
        memory_model: "The value remains alive. Only operations that could invalidate or race with the live view are restricted.",
        why: "A reference must keep pointing to valid, consistently accessed data for its entire live region.",
        misconception: "Borrowed does not mean dead or moved.",
        checkpoint: "Is a value dead merely because an `&T` points to it?",
        choices: ["No, it is alive with restricted access", "Yes"],
        correct_choice: 0,
        related: &["exclusive_borrow", "non_lexical_lifetimes", "lifetimes"],
    },
    ConceptLesson {
        id: "exclusive_borrow",
        title: "Exclusive mutable borrowing",
        one_line: "An `&mut T` promises that its access path is exclusive while the borrow is live.",
        rule: "Do not read, write, or create another overlapping borrow through competing paths until it ends.",
        memory_model: "The owner is still alive, but access is temporarily delegated to the mutable reference.",
        why: "Exclusivity makes mutation deterministic and prevents data races.",
        misconception: "`&mut` does not own or automatically drop the referent.",
        checkpoint: "Can two live `&mut` references overlap the same value?",
        choices: ["No", "Yes, on one thread"],
        correct_choice: 0,
        related: &["borrow_conflicts", "non_lexical_lifetimes"],
    },
    ConceptLesson {
        id: "moves_and_borrows",
        title: "Moving while borrowed",
        one_line: "Ownership cannot move while a reference still relies on the old owner's storage contract.",
        rule: "Finish using the reference before moving, or return/store owned data instead.",
        memory_model: "The borrow is tied to the referent and its valid owner-controlled lifetime.",
        why: "Moving may invalidate location, cleanup, or aliasing assumptions represented by the reference.",
        misconception: "A reference does not silently follow every ownership transformation.",
        checkpoint: "May an owner be moved while one of its fields is still borrowed and later used?",
        choices: ["No", "Always"],
        correct_choice: 0,
        related: &["moves", "borrow_conflicts", "lifetimes"],
    },
    ConceptLesson {
        id: "non_lexical_lifetimes",
        title: "Non-lexical lifetimes",
        one_line: "A borrow normally ends after its last possible use, not automatically at the closing brace.",
        rule: "Control flow determines the live region that the borrow checker must protect.",
        memory_model: "The reference binding can remain in scope even after its borrow is no longer live.",
        why: "Ending protection at the final use permits safe mutation sooner.",
        misconception: "Scope and borrow lifetime are related but not identical.",
        checkpoint: "Can a borrow end before its variable goes out of lexical scope?",
        choices: ["Yes", "No"],
        correct_choice: 0,
        related: &["borrow_conflicts", "lifetimes"],
    },
    ConceptLesson {
        id: "mutability_contracts",
        title: "Mutability contracts",
        one_line: "Mutation requires a mutable owner path, exclusive borrow, or an explicit interior-mutability API.",
        rule: "Every layer from the binding to the changed value must provide the required access.",
        memory_model: "`mut` changes what a binding may do; `&mut` delegates exclusive access; wrappers can enforce access at runtime.",
        why: "The type signature communicates where observable state may change.",
        misconception: "Making the outer variable `mut` cannot give `Rc<T>` a missing `DerefMut` implementation.",
        checkpoint: "Does `Rc<T>` alone permit mutable access to `T`?",
        choices: ["No", "Yes, when the Rc binding is mut"],
        correct_choice: 0,
        related: &["method_receivers", "interior_mutability", "smart_pointers"],
    },
    ConceptLesson {
        id: "method_receivers",
        title: "Method receiver contracts",
        one_line: "`&self`, `&mut self`, and `self` mean shared access, exclusive access, and ownership transfer.",
        rule: "The call site must supply the receiver access written in the resolved method signature.",
        memory_model: "Method syntax may auto-borrow or auto-dereference, but it cannot invent access a wrapper does not provide.",
        why: "Receiver types make state changes and consumption visible to callers.",
        misconception: "A method name such as `clear` is not inherently mutable; its resolved signature proves the contract.",
        checkpoint: "What receiver normally permits changing a Vec in place?",
        choices: ["&mut self", "&self"],
        correct_choice: 0,
        related: &["mutability_contracts", "method_resolution"],
    },
    ConceptLesson {
        id: "lifetimes",
        title: "Lifetimes",
        one_line: "A lifetime describes how long a reference may remain valid relative to its referent.",
        rule: "No reference may outlive the value and storage it points into.",
        memory_model: "Lifetime annotations connect existing regions; they do not extend runtime storage.",
        why: "The relationship prevents dangling pointers without garbage collection.",
        misconception: "Adding `'static` does not make a local value live forever.",
        checkpoint: "Can a lifetime annotation extend the runtime life of a local String?",
        choices: ["No", "Yes"],
        correct_choice: 0,
        related: &[
            "returning_references",
            "temporary_lifetimes",
            "non_lexical_lifetimes",
        ],
    },
    ConceptLesson {
        id: "returning_references",
        title: "Returning references",
        one_line: "A returned reference must point into storage that remains alive in the caller.",
        rule: "Tie the output to an input reference, or return an owned value.",
        memory_model: "Local stack variables and their owned allocations are cleaned up when the function returns.",
        why: "A reference into cleaned-up local state would dangle.",
        misconception: "Heap bytes owned by a local `String` do not outlive the local owner automatically.",
        checkpoint: "May a function safely return `&str` into a locally created String?",
        choices: ["No", "Yes, because String uses the heap"],
        correct_choice: 0,
        related: &["lifetimes", "ownership"],
    },
    ConceptLesson {
        id: "temporary_lifetimes",
        title: "Temporary lifetimes",
        one_line: "A reference cannot remain in use after the temporary owner is dropped.",
        rule: "Bind the owner to a local variable when its borrowed result must live longer.",
        memory_model: "Temporaries have compiler-defined drop points, often at the end of a statement.",
        why: "The referent must still exist at every use of the reference.",
        misconception: "Borrowing a temporary does not automatically promote every temporary to the enclosing scope.",
        checkpoint: "Can naming the owner extend how long its borrowed data remains available?",
        choices: ["Yes", "No"],
        correct_choice: 0,
        related: &["lifetimes", "drop"],
    },
    ConceptLesson {
        id: "trait_bounds",
        title: "Trait requirements",
        one_line: "A generic operation is available only when the concrete type satisfies every required trait bound.",
        rule: "Implement, import, or constrain traits deliberately; do not assume similarly shaped types are interchangeable.",
        memory_model: "Traits describe compile-time capabilities and may select static or dynamic dispatch.",
        why: "The compiler must prove the called behavior exists for every permitted type.",
        misconception: "E0277 does not always mean writing an impl; changing ownership or threading representation may be correct.",
        checkpoint: "Does a generic function compile if one allowed type lacks a required bound?",
        choices: ["No", "Yes, until that type is used"],
        correct_choice: 0,
        related: &["method_resolution", "send_sync", "type_mismatches"],
    },
    ConceptLesson {
        id: "send_sync",
        title: "Send and Sync",
        one_line: "`Send` permits moving a value between threads; `Sync` permits sharing `&T` between threads.",
        rule: "Thread boundaries require every captured component to satisfy the executor or thread API's bounds.",
        memory_model: "`Rc` and `RefCell` use non-thread-safe runtime state; `Arc` and locks provide thread-aware mechanisms.",
        why: "These auto traits prevent data races hidden inside composed types.",
        misconception: "Replacing `Rc` with `Arc` does not make the inner value mutable or automatically `Sync`.",
        checkpoint: "Which shared-owner type uses atomic reference counts?",
        choices: ["Arc", "Rc"],
        correct_choice: 0,
        related: &["smart_pointers", "interior_mutability", "async_futures"],
    },
    ConceptLesson {
        id: "type_mismatches",
        title: "Type mismatches",
        one_line: "The produced type and the context's expected type describe different contracts.",
        rule: "Change the producer, consumer, or use an explicit valid conversion that preserves intent.",
        memory_model: "References, owners, options, results, and wrappers are distinct representations with distinct behavior.",
        why: "Explicit conversions prevent accidental allocation, failure, truncation, or ownership changes.",
        misconception: "Types with similar printed values are not automatically interchangeable.",
        checkpoint: "Should a fallible conversion normally expose its failure path?",
        choices: ["Yes", "No"],
        correct_choice: 0,
        related: &["trait_bounds", "ownership"],
    },
    ConceptLesson {
        id: "method_resolution",
        title: "Method resolution",
        one_line: "A method comes from an inherent impl or an in-scope trait for a compatible receiver type.",
        rule: "Check the resolved receiver layer, trait import, bounds, and auto-deref path.",
        memory_model: "Method-call syntax searches candidate receiver forms but cannot bypass ownership or trait rules.",
        why: "Resolution must identify one unambiguous function and prove its receiver contract.",
        misconception: "A method existing on `T` does not guarantee it is mutable through `Rc<T>`.",
        checkpoint: "Can importing a trait make its implemented methods available?",
        choices: ["Yes", "No"],
        correct_choice: 0,
        related: &["trait_bounds", "method_receivers"],
    },
    ConceptLesson {
        id: "closure_captures",
        title: "Closure captures",
        one_line: "A closure may borrow or own values from the surrounding scope.",
        rule: "Use `move` when the closure must outlive the borrowed environment, then decide which captures need cloning.",
        memory_model: "The compiler creates an anonymous environment struct containing references or owned fields.",
        why: "The environment must remain valid for as long as the closure can run.",
        misconception: "`move` changes capture ownership; it does not force Copy values to become heap allocated.",
        checkpoint: "What does a `move` closure normally do with non-Copy captures?",
        choices: ["Takes ownership", "Creates hidden references"],
        correct_choice: 0,
        related: &["lifetimes", "moves", "send_sync"],
    },
    ConceptLesson {
        id: "async_futures",
        title: "Async futures",
        one_line: "Calling an async function creates a future whose state is advanced when polled.",
        rule: "Values held across `.await` become part of the future and must satisfy its lifetime and thread bounds.",
        memory_model: "The future is a state machine storing locals needed after suspension points.",
        why: "Suspension lets other work run while preserving enough state to resume safely.",
        misconception: "Calling an async function does not run its body to completion immediately.",
        checkpoint: "Can a local held across `.await` become stored in the future state?",
        choices: ["Yes", "No"],
        correct_choice: 0,
        related: &["send_sync", "closure_captures", "lifetimes"],
    },
    ConceptLesson {
        id: "smart_pointers",
        title: "Box, Rc, and Arc",
        one_line: "`Box` owns once, `Rc` shares on one thread, and `Arc` shares with atomic counts.",
        rule: "Choose a wrapper from the ownership topology, not merely to silence a move error.",
        memory_model: "The stack handle points to heap storage; reference-counted allocations also store owner counters.",
        why: "Different sharing models have different guarantees and runtime costs.",
        misconception: "Shared ownership does not imply shared mutation.",
        checkpoint: "Which type represents one owner of a heap allocation?",
        choices: ["Box", "Rc"],
        correct_choice: 0,
        related: &["ownership", "interior_mutability", "send_sync"],
    },
    ConceptLesson {
        id: "interior_mutability",
        title: "Interior mutability",
        one_line: "Interior-mutability types permit mutation through shared access by enforcing rules at runtime or through restricted operations.",
        rule: "Use `Cell` for Copy-style replacement, `RefCell` for single-threaded dynamic borrows, and locks for synchronized access.",
        memory_model: "The wrapper stores runtime state such as a borrow flag or lock next to the value.",
        why: "Some ownership topologies cannot express mutation with ordinary exclusive references at the API boundary.",
        misconception: "`RefCell` does not remove borrowing rules; violations panic at runtime.",
        checkpoint: "When does `RefCell` check borrow compatibility?",
        choices: ["At runtime", "Never"],
        correct_choice: 0,
        related: &["mutability_contracts", "smart_pointers", "send_sync"],
    },
    ConceptLesson {
        id: "reinitialization",
        title: "Reinitialization",
        one_line: "Assigning a new value can make a previously moved place available again.",
        rule: "The place must be assignable without reading or dropping an unavailable old value.",
        memory_model: "The availability bit for that place changes from moved to initialized with a new owner value.",
        why: "Rust tracks whether each place currently contains a value that must eventually be dropped.",
        misconception: "A moved binding is not permanently unusable when it can legally receive a new value.",
        checkpoint: "Can assigning a new String make a moved local usable again?",
        choices: ["Yes", "No"],
        correct_choice: 0,
        related: &["moves", "partial_moves", "drop"],
    },
    ConceptLesson {
        id: "drop",
        title: "Drop and cleanup",
        one_line: "Rust runs cleanup exactly once when the current owner leaves its initialized lifetime.",
        rule: "Moved-out places are not dropped; reinitialized places drop their new value normally.",
        memory_model: "Drop responsibility follows ownership and may recursively clean up fields and heap allocations.",
        why: "Deterministic cleanup releases files, locks, allocations, and other resources without double free.",
        misconception: "Drop always means heap deallocation; many values have no heap storage and custom Drop may release other resources.",
        checkpoint: "Is a moved-out value dropped again through its old binding?",
        choices: ["No", "Yes"],
        correct_choice: 0,
        related: &["ownership", "moves", "reinitialization"],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_problem_family_has_a_complete_beginner_narrative() {
        let categories = [
            "use_after_move",
            "partial_move",
            "multiple_mutable_borrows",
            "mutable_while_shared",
            "move_while_borrowed",
            "assign_while_borrowed",
            "use_while_mutably_borrowed",
            "move_out_of_borrowed_content",
            "immutable_mutation",
            "missing_lifetime",
            "returning_local_reference",
            "borrowed_value_too_short",
            "temporary_dropped_while_borrowed",
            "trait_requirement",
            "type_mismatch",
            "method_or_trait_unavailable",
            "closure_may_outlive_borrow",
            "borrowed_data_escapes",
            "await_outside_async",
            "recursive_async_function",
        ];

        for category in categories {
            let narrative = beginner_narrative(category, "value");
            assert!(!narrative.title.is_empty(), "missing title for {category}");
            assert!(!narrative.before.is_empty(), "missing before for {category}");
            assert!(
                !narrative.operation.is_empty(),
                "missing operation for {category}"
            );
            assert!(!narrative.now.is_empty(), "missing now for {category}");
            assert!(
                !narrative.common_expectation.is_empty(),
                "missing comparison for {category}"
            );
            assert!(
                !narrative.rust_model.is_empty(),
                "missing Rust model for {category}"
            );
            assert!(
                !narrative.intuition.is_empty(),
                "missing intuition for {category}"
            );
            assert!(
                !narrative.misconception.is_empty(),
                "missing misconception for {category}"
            );
            assert!(
                narrative.vocabulary.len() >= 3,
                "missing vocabulary for {category}"
            );
        }
    }

    #[test]
    fn fallback_narrative_remains_useful_for_new_problem_families() {
        let narrative = beginner_narrative("future_diagnostic", "item");
        assert!(narrative.title.contains("item"));
        assert_eq!(narrative.vocabulary.len(), 3);
    }
}
