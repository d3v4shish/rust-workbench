//! Driver support for compiling conservative borrow-checker repairs through a source overlay.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rustc_interface::interface;
use rustc_session::borrowck_repair::{
    AUTOFIX_CHILD_ENV, AUTOFIX_OVERLAY_ENV, AUTOFIX_PLAN_ENV, BorrowckOwnershipEventKind,
    BorrowckWrapperStrategy, SerializedBorrowckBinding, SerializedBorrowckOwnershipEvent,
    SerializedBorrowckRepairEdit, SerializedBorrowckRepairPlan, SerializedBorrowckSpan,
    SerializedBorrowckWrapperVariant,
};
use rustc_span::source_map::{FileLoader, RealFileLoader};
use serde_json::{Value, json};

const MAX_REPAIR_ROUNDS: usize = 4;
const MAX_EDITOR_WRAPPER_VARIANTS: usize = 12;

pub(crate) enum AutofixOutcome {
    CompilationFailed,
    Success,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct ValidatedWrapperSuggestion {
    bindings: Vec<SerializedBorrowckBinding>,
    triggers: Vec<SerializedBorrowckSpan>,
    strategies: Vec<BorrowckWrapperStrategy>,
    edits: Vec<SerializedBorrowckRepairEdit>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct CachedWrapperSuggestions {
    format_version: u32,
    compiler_identity: String,
    suggestions: Vec<ValidatedWrapperSuggestion>,
}

pub(crate) fn should_orchestrate() -> bool {
    env::var_os(AUTOFIX_CHILD_ENV).is_none()
        && (env::var_os("CARGO_MANIFEST_DIR").is_none()
            || env::var_os("CARGO_PRIMARY_PACKAGE").is_some())
}

pub(crate) fn install_overlay(config: &mut interface::Config) -> io::Result<()> {
    let Some(overlay_path) = env::var_os(AUTOFIX_OVERLAY_ENV).map(PathBuf::from) else {
        return Ok(());
    };
    let serialized: BTreeMap<String, String> =
        serde_json::from_reader(File::open(overlay_path)?).map_err(io::Error::other)?;
    let overlay = serialized
        .into_iter()
        .map(|(original, patched)| (PathBuf::from(original), PathBuf::from(patched)))
        .collect();
    config.file_loader = Some(Box::new(OverlayFileLoader { overlay }));
    Ok(())
}

/// Runs the user's compilation unchanged, validates ownership-wrapper variants in temporary
/// overlays, and emits only the variants whose exact edits compile successfully.
pub(crate) fn run_suggestions(args: &[String]) -> io::Result<AutofixOutcome> {
    let work_dir = WorkDir::create()?;
    let plan_path = work_dir.path().join("suggestions.json");
    let output = run_child(args, &plan_path, None)?;
    let plan = read_plan(&plan_path)?;

    let suggestions = match &plan {
        Some(plan) if wrapper_suggestions_requested(args) => {
            cached_or_validate_wrapper_suggestions(args, plan, work_dir.path())?
        }
        _ => Vec::new(),
    };

    replay_output(&output)?;
    if let Some(plan) = &plan {
        emit_wrapper_suggestions(args, plan, &suggestions)?;
        emit_ownership_model(args, plan)?;
        emit_ownership_events(args, plan)?;
    }

    Ok(if output.status.success() {
        AutofixOutcome::Success
    } else {
        AutofixOutcome::CompilationFailed
    })
}

fn wrapper_suggestions_requested(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "-Zborrowck-wrapper-suggestions")
        || args.windows(2).any(|args| {
            args[0] == "-Z" && args[1].trim_start_matches('=') == "borrowck-wrapper-suggestions"
        })
}

fn cached_or_validate_wrapper_suggestions(
    args: &[String],
    plan: &SerializedBorrowckRepairPlan,
    work_dir: &Path,
) -> io::Result<Vec<ValidatedWrapperSuggestion>> {
    let cache_path = wrapper_validation_cache_path(args, plan)?;
    let compiler_identity = compiler_cache_identity();
    if let Some(cache_path) = &cache_path
        && let Ok(file) = File::open(cache_path)
        && let Ok(cached) = serde_json::from_reader::<_, CachedWrapperSuggestions>(file)
        && cached.format_version == 2
        && cached.compiler_identity == compiler_identity
    {
        return Ok(cached.suggestions);
    }

    let suggestions = validate_wrapper_suggestions(args, plan, work_dir)?;
    if let Some(cache_path) = cache_path {
        if let Some(directory) = cache_path.parent() {
            fs::create_dir_all(directory)?;
            prune_wrapper_validation_cache(directory, 128);
        }
        let temporary = cache_path.with_extension(format!("json.{}.tmp", std::process::id()));
        let cached = CachedWrapperSuggestions { format_version: 2, compiler_identity, suggestions };
        serde_json::to_writer(File::create(&temporary)?, &cached).map_err(io::Error::other)?;
        fs::rename(&temporary, &cache_path)?;
        return Ok(cached.suggestions);
    }
    Ok(suggestions)
}

fn wrapper_validation_cache_path(
    args: &[String],
    plan: &SerializedBorrowckRepairPlan,
) -> io::Result<Option<PathBuf>> {
    let Some(model_directory) = ownership_model_directory(args) else { return Ok(None) };
    // The source text, proposed edits, dependency artifact arguments, target configuration, and
    // compiler revision all participate. A cache hit therefore means the exact candidate that was
    // previously accepted is being requested again; source or dependency changes revalidate it.
    let key = serde_json::to_vec(&json!({
        "format_version": 2,
        "compiler_identity": compiler_cache_identity(),
        "args": args,
        "crate_name": &plan.crate_name,
        "stable_crate_id": plan.stable_crate_id,
        "sources": &plan.sources,
        "repairs": &plan.repairs,
        "wrapper_variants": &plan.wrapper_variants,
    }))
    .map_err(io::Error::other)?;
    Ok(Some(
        model_directory
            .join(".validation-cache")
            .join(format!("{}.cache", stable_bytes_hash(&key))),
    ))
}

fn compiler_cache_identity() -> String {
    let executable = env::current_exe()
        .ok()
        .and_then(|path| fs::metadata(path).ok())
        .map(|metadata| {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            format!("{}:{modified}", metadata.len())
        })
        .unwrap_or_else(|| "unavailable".to_owned());
    format!(
        "{}:{}:{}:{}:{executable}",
        option_env!("CFG_VERSION").unwrap_or("unknown"),
        option_env!("CFG_VER_HASH").unwrap_or("unknown"),
        option_env!("CFG_VER_DATE").unwrap_or("unknown"),
        option_env!("CFG_RELEASE").unwrap_or("unknown"),
    )
}

fn stable_bytes_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn prune_wrapper_validation_cache(directory: &Path, maximum_entries: usize) {
    let Ok(entries) = fs::read_dir(directory) else { return };
    let mut entries = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "cache"))
        .collect::<Vec<_>>();
    if entries.len() < maximum_entries {
        return;
    }
    entries.sort_by_key(|entry| entry.metadata().and_then(|metadata| metadata.modified()).ok());
    let remove_count = entries.len().saturating_sub(maximum_entries - 1);
    for entry in entries.into_iter().take(remove_count) {
        let _ = fs::remove_file(entry.path());
    }
}

fn emit_ownership_events(args: &[String], plan: &SerializedBorrowckRepairPlan) -> io::Result<()> {
    if plan.ownership_events.is_empty()
        || ownership_model_directory(args).is_some()
        || !uses_json_error_format(args)
    {
        return Ok(());
    }

    let mut stderr = io::stderr().lock();
    for event in &plan.ownership_events {
        let source =
            plan.sources.iter().find(|source| source.path == event.path).ok_or_else(|| {
                io::Error::other("ownership event source was not included in plan")
            })?;
        let source_text = source
            .source
            .as_deref()
            .ok_or_else(|| io::Error::other("ownership event source text was not included"))?;
        let (byte_start, byte_end) = narrow_event_to_binding(event, source_text);
        let source_hash = stable_source_hash(source_text);
        let payload = json!({
            "version": 1,
            "kind": event.kind,
            "name": event.binding.name,
            "source_hash": source_hash,
            "binding_byte_start": event.binding.byte_start,
            "detail": event.detail,
        });
        let code = ownership_event_code(event.kind);
        let diagnostic = json!({
            "$message_type": "diagnostic",
            "message": payload.to_string(),
            "code": { "code": code, "explanation": Value::Null },
            "level": "note",
            "spans": [diagnostic_span(
                plan,
                &event.path,
                byte_start,
                byte_end,
                true,
                Some("ownership event"),
                None,
            )?],
            "children": [],
            "rendered": Value::Null,
        });
        serde_json::to_writer(&mut stderr, &diagnostic).map_err(io::Error::other)?;
        writeln!(stderr)?;
    }
    Ok(())
}

fn emit_ownership_model(args: &[String], plan: &SerializedBorrowckRepairPlan) -> io::Result<()> {
    let Some(directory) = ownership_model_directory(args) else { return Ok(()) };
    if plan.ownership_events.is_empty() {
        return Ok(());
    }

    fs::create_dir_all(&directory)?;
    let file_name = format!("{}-{:016x}.json", plan.crate_name, plan.stable_crate_id);
    let model_path = directory.join(file_name);
    let temporary_path = model_path.with_extension(format!("json.{}.tmp", std::process::id()));
    serde_json::to_writer_pretty(File::create(&temporary_path)?, plan).map_err(io::Error::other)?;
    fs::rename(&temporary_path, &model_path)?;

    if !uses_json_error_format(args) {
        return Ok(());
    }
    let event = &plan.ownership_events[0];
    let payload = json!({
        "version": plan.schema_version,
        "path": model_path,
        "crate_name": plan.crate_name,
    });
    let diagnostic = json!({
        "$message_type": "diagnostic",
        "message": payload.to_string(),
        "code": { "code": "borrowck_ownership_model", "explanation": Value::Null },
        "level": "note",
        "spans": [diagnostic_span(
            plan,
            &event.path,
            event.byte_start,
            event.byte_end,
            true,
            Some("compiler ownership model"),
            None,
        )?],
        "children": [],
        "rendered": Value::Null,
    });
    let mut stderr = io::stderr().lock();
    serde_json::to_writer(&mut stderr, &diagnostic).map_err(io::Error::other)?;
    writeln!(stderr)
}

fn ownership_model_directory(args: &[String]) -> Option<PathBuf> {
    args.iter().find_map(|arg| arg.strip_prefix("-Zborrowck-ownership-model=").map(PathBuf::from))
}

fn ownership_event_code(kind: BorrowckOwnershipEventKind) -> &'static str {
    match kind {
        BorrowckOwnershipEventKind::BorrowActivate => "borrowck_ownership_borrow_activate",
        BorrowckOwnershipEventKind::BorrowEnd => "borrowck_ownership_borrow_end",
        BorrowckOwnershipEventKind::BorrowMutable => "borrowck_ownership_borrow_mutable",
        BorrowckOwnershipEventKind::BorrowShared => "borrowck_ownership_borrow_shared",
        BorrowckOwnershipEventKind::Clone => "borrowck_ownership_clone",
        BorrowckOwnershipEventKind::Copy => "borrowck_ownership_copy",
        BorrowckOwnershipEventKind::Drop => "borrowck_ownership_drop",
        BorrowckOwnershipEventKind::LastUse => "borrowck_ownership_last_use",
        BorrowckOwnershipEventKind::Move => "borrowck_ownership_move",
        BorrowckOwnershipEventKind::PartialMove => "borrowck_ownership_partial_move",
        BorrowckOwnershipEventKind::Reinitialize => "borrowck_ownership_reinitialize",
    }
}

fn narrow_event_to_binding(
    event: &SerializedBorrowckOwnershipEvent,
    source: &str,
) -> (usize, usize) {
    if event.byte_start <= event.byte_end
        && event.byte_end <= source.len()
        && source.is_char_boundary(event.byte_start)
        && source.is_char_boundary(event.byte_end)
        && let Some(relative) = source[event.byte_start..event.byte_end].rfind(&event.binding.name)
    {
        let start = event.byte_start + relative;
        return (start, start + event.binding.name.len());
    }
    (event.byte_start, event.byte_end)
}

fn stable_source_hash(source: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn validate_wrapper_suggestions(
    args: &[String],
    plan: &SerializedBorrowckRepairPlan,
    work_dir: &Path,
) -> io::Result<Vec<ValidatedWrapperSuggestion>> {
    let variants: Vec<_> =
        plan.wrapper_variants.iter().take(MAX_EDITOR_WRAPPER_VARIANTS).cloned().collect();
    let mut suggestions = Vec::new();

    for (index, variant) in variants.iter().enumerate() {
        if let Some(suggestion) = validate_wrapper_suggestion_set(
            args,
            plan,
            work_dir,
            &format!("variant-{index}"),
            std::slice::from_ref(variant),
        )? {
            suggestions.push(suggestion);
        }
    }
    if variants.len() > 1
        && let Some(suggestion) =
            validate_wrapper_suggestion_set(args, plan, work_dir, "all-wrappers", &variants)?
    {
        suggestions.push(suggestion);
    }
    Ok(suggestions)
}

fn validate_wrapper_suggestion_set(
    args: &[String],
    plan: &SerializedBorrowckRepairPlan,
    work_dir: &Path,
    name: &str,
    variants: &[SerializedBorrowckWrapperVariant],
) -> io::Result<Option<ValidatedWrapperSuggestion>> {
    let candidate_dir = work_dir.join(name);
    let sources_dir = candidate_dir.join("sources");
    let artifacts_dir = candidate_dir.join("artifacts");
    fs::create_dir_all(&sources_dir)?;
    fs::create_dir_all(&artifacts_dir)?;

    let bindings: Vec<_> = variants.iter().map(|variant| variant.binding.clone()).collect();
    let triggers: Vec<_> = variants.iter().map(|variant| variant.trigger.clone()).collect();
    let mut edits: Vec<_> = variants.iter().flat_map(|variant| variant.edits.clone()).collect();
    for repair in &plan.repairs {
        if repair
            .binding
            .as_ref()
            .is_some_and(|binding| bindings.iter().any(|wrapper| same_binding(binding, wrapper)))
        {
            continue;
        }
        edits.extend(repair.edits.clone());
    }
    let edits = match normalize_variant_edits(plan, edits) {
        Ok(edits) if !edits.is_empty() => edits,
        Ok(_) | Err(_) => return Ok(None),
    };
    let overlay = match materialize_variant_sources(plan, &sources_dir, edits.clone()) {
        Ok(overlay) => overlay,
        Err(_) => return Ok(None),
    };
    let overlay_path = write_overlay(&candidate_dir, 0, &overlay)?;
    let child_args = redirect_variant_outputs(args, &artifacts_dir)?;
    let candidate_plan = candidate_dir.join("compile.json");
    let output = run_child(&child_args, &candidate_plan, overlay_path.as_deref())?;
    if !output.status.success() {
        return Ok(None);
    }

    Ok(Some(ValidatedWrapperSuggestion {
        bindings,
        triggers,
        strategies: variants.iter().map(|variant| variant.strategy).collect(),
        edits,
    }))
}

fn emit_wrapper_suggestions(
    args: &[String],
    plan: &SerializedBorrowckRepairPlan,
    suggestions: &[ValidatedWrapperSuggestion],
) -> io::Result<()> {
    if suggestions.is_empty() {
        return Ok(());
    }
    if !uses_json_error_format(args) {
        let mut stderr = io::stderr().lock();
        for suggestion in suggestions {
            writeln!(stderr, "help: {}", wrapper_suggestion_title(&suggestion.strategies))?;
        }
        return Ok(());
    }

    let mut stderr = io::stderr().lock();
    for suggestion in suggestions {
        let code = wrapper_suggestion_code(&suggestion.strategies);
        let title = wrapper_suggestion_title(&suggestion.strategies);
        let primary_spans = suggestion
            .bindings
            .iter()
            .enumerate()
            .map(|(index, binding)| {
                diagnostic_span(
                    plan,
                    &binding.path,
                    binding.byte_start,
                    binding.byte_start + binding.name.len(),
                    index == 0,
                    Some("ownership wrapper candidate"),
                    None,
                )
            })
            .collect::<io::Result<Vec<_>>>()?;
        let trigger_spans = suggestion
            .triggers
            .iter()
            .map(|trigger| {
                diagnostic_span(
                    plan,
                    &trigger.path,
                    trigger.byte_start,
                    trigger.byte_end,
                    true,
                    Some("ownership error occurs here"),
                    None,
                )
            })
            .collect::<io::Result<Vec<_>>>()?;
        let primary_spans = primary_spans.into_iter().chain(trigger_spans).collect::<Vec<_>>();
        let edit_spans = suggestion
            .edits
            .iter()
            .map(|edit| {
                diagnostic_span(
                    plan,
                    &edit.path,
                    edit.byte_start,
                    edit.byte_end,
                    true,
                    None,
                    Some(&edit.replacement),
                )
            })
            .collect::<io::Result<Vec<_>>>()?;
        let diagnostic = json!({
            "$message_type": "diagnostic",
            "message": "compiler validated an ownership-wrapper rewrite; review its runtime semantics",
            "code": { "code": code, "explanation": Value::Null },
            "level": "help",
            "spans": primary_spans,
            "children": [{
                "message": title,
                "code": { "code": code, "explanation": Value::Null },
                "level": "help",
                "spans": edit_spans,
                "children": [],
                "rendered": Value::Null,
            }],
            "rendered": format!("help: {title}\n"),
        });
        serde_json::to_writer(&mut stderr, &diagnostic).map_err(io::Error::other)?;
        writeln!(stderr)?;
    }
    Ok(())
}

fn uses_json_error_format(args: &[String]) -> bool {
    args.iter().any(|arg| arg.starts_with("--error-format=json"))
        || args.windows(2).any(|args| args[0] == "--error-format" && args[1].starts_with("json"))
}

fn wrapper_suggestion_code(strategies: &[BorrowckWrapperStrategy]) -> &'static str {
    match strategies {
        [BorrowckWrapperStrategy::Arc] => "borrowck_wrapper_arc",
        [BorrowckWrapperStrategy::ArcMutex] => "borrowck_wrapper_arc_mutex",
        [BorrowckWrapperStrategy::ArcRwLock] => "borrowck_wrapper_arc_rw_lock",
        [BorrowckWrapperStrategy::Mutex] => "borrowck_wrapper_mutex",
        [BorrowckWrapperStrategy::Rc] => "borrowck_wrapper_rc",
        [BorrowckWrapperStrategy::RefCell] => "borrowck_wrapper_ref_cell",
        [BorrowckWrapperStrategy::RcRefCell] => "borrowck_wrapper_rc_ref_cell",
        [BorrowckWrapperStrategy::RwLock] => "borrowck_wrapper_rw_lock",
        _ => "borrowck_wrapper_all",
    }
}

fn wrapper_suggestion_title(strategies: &[BorrowckWrapperStrategy]) -> &'static str {
    match strategies {
        [BorrowckWrapperStrategy::Arc] => {
            "Use Arc for thread-safe shared ownership (compiler validated)"
        }
        [BorrowckWrapperStrategy::ArcMutex] => {
            "Use Arc<Mutex<_>> for thread-safe shared mutation (compiler validated)"
        }
        [BorrowckWrapperStrategy::ArcRwLock] => {
            "Use Arc<RwLock<_>> for read-heavy shared mutation (compiler validated)"
        }
        [BorrowckWrapperStrategy::Mutex] => {
            "Use Mutex for synchronized interior mutability (compiler validated)"
        }
        [BorrowckWrapperStrategy::Rc] => "Use Rc for shared ownership (compiler validated)",
        [BorrowckWrapperStrategy::RefCell] => {
            "Use RefCell for interior mutability (compiler validated)"
        }
        [BorrowckWrapperStrategy::RcRefCell] => {
            "Use Rc<RefCell<_>> for shared mutable ownership (compiler validated)"
        }
        [BorrowckWrapperStrategy::RwLock] => {
            "Use RwLock for synchronized interior mutability (compiler validated)"
        }
        _ => "Apply all validated ownership-wrapper changes",
    }
}

fn diagnostic_span(
    plan: &SerializedBorrowckRepairPlan,
    path: &Path,
    byte_start: usize,
    byte_end: usize,
    is_primary: bool,
    label: Option<&str>,
    suggested_replacement: Option<&str>,
) -> io::Result<Value> {
    let source = plan
        .sources
        .iter()
        .find(|source| source.path == path)
        .ok_or_else(|| io::Error::other("suggestion source was not included in the plan"))?;
    let source = source
        .source
        .as_deref()
        .ok_or_else(|| io::Error::other("suggestion source text was not included in the plan"))?;
    if byte_start > byte_end
        || byte_end > source.len()
        || !source.is_char_boundary(byte_start)
        || !source.is_char_boundary(byte_end)
    {
        return Err(io::Error::other("suggestion span has invalid UTF-8 byte boundaries"));
    }
    let (line_start, column_start) = line_and_column(source, byte_start);
    let (line_end, column_end) = line_and_column(source, byte_end);
    Ok(json!({
        "file_name": path_to_utf8(path)?,
        "byte_start": byte_start,
        "byte_end": byte_end,
        "line_start": line_start,
        "line_end": line_end,
        "column_start": column_start,
        "column_end": column_end,
        "is_primary": is_primary,
        "text": [],
        "label": label,
        "suggested_replacement": suggested_replacement,
        "suggestion_applicability": suggested_replacement.map(|_| "MaybeIncorrect"),
        "expansion": Value::Null,
    }))
}

fn line_and_column(source: &str, offset: usize) -> (usize, usize) {
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let column = source[line_start..offset].chars().count() + 1;
    (line, column)
}

pub(crate) fn run(args: &[String], output_root: &Path) -> io::Result<AutofixOutcome> {
    let output_root = absolute_path(output_root)?;
    fs::create_dir_all(&output_root)?;
    let work_dir = WorkDir::create()?;

    let mut overlay = BTreeMap::new();
    let mut crate_dir = None;
    let mut crate_identity = None;
    let mut applied_repairs = Vec::new();
    let mut skipped_repairs = Vec::new();
    let mut wrapper_seed_plan = None;

    for round in 0..=MAX_REPAIR_ROUNDS {
        let plan_path = work_dir.path().join(format!("round-{round}.json"));
        let overlay_path = write_overlay(work_dir.path(), round, &overlay)?;
        let output = run_child(args, &plan_path, overlay_path.as_deref())?;
        let plan = read_plan(&plan_path)?;

        if let Some(plan) = &plan {
            let identity = (plan.crate_name.clone(), plan.stable_crate_id);
            if let Some(expected) = &crate_identity
                && expected != &identity
            {
                replay_output(&output)?;
                return Err(io::Error::other("borrowck autofix child changed crate identity"));
            }
            crate_identity = Some(identity);
            crate_dir.get_or_insert_with(|| crate_output_dir(&output_root, plan));
            if wrapper_seed_plan.is_none()
                && (!plan.wrapper_variants.is_empty() || !plan.wrapper_rejections.is_empty())
            {
                wrapper_seed_plan = Some(plan.clone());
            }
        }

        if output.status.success() {
            if let (Some(plan), Some(crate_dir)) = (&plan, &crate_dir) {
                let wrapper_results =
                    compile_wrapper_variants(args, wrapper_seed_plan.as_ref(), crate_dir)?;
                write_repair_manifest(
                    crate_dir,
                    plan,
                    "success",
                    round,
                    &overlay,
                    &applied_repairs,
                    &skipped_repairs,
                    wrapper_seed_plan.as_ref(),
                    &wrapper_results,
                )?;
            }
            replay_output(&output)?;
            return Ok(AutofixOutcome::Success);
        }

        let Some(plan) = plan else {
            replay_output(&output)?;
            return Ok(AutofixOutcome::CompilationFailed);
        };
        let Some(current_crate_dir) = crate_dir.as_deref() else {
            replay_output(&output)?;
            return Ok(AutofixOutcome::CompilationFailed);
        };

        if round == MAX_REPAIR_ROUNDS || plan.repairs.is_empty() {
            let wrapper_results =
                compile_wrapper_variants(args, wrapper_seed_plan.as_ref(), current_crate_dir)?;
            write_repair_manifest(
                current_crate_dir,
                &plan,
                "failed",
                round,
                &overlay,
                &applied_repairs,
                &skipped_repairs,
                wrapper_seed_plan.as_ref(),
                &wrapper_results,
            )?;
            replay_output(&output)?;
            return Ok(AutofixOutcome::CompilationFailed);
        }

        let applied = apply_plan(
            &plan,
            current_crate_dir,
            round + 1,
            &mut overlay,
            &mut applied_repairs,
            &mut skipped_repairs,
        )?;
        if applied == 0 {
            let wrapper_results =
                compile_wrapper_variants(args, wrapper_seed_plan.as_ref(), current_crate_dir)?;
            write_repair_manifest(
                current_crate_dir,
                &plan,
                "failed",
                round,
                &overlay,
                &applied_repairs,
                &skipped_repairs,
                wrapper_seed_plan.as_ref(),
                &wrapper_results,
            )?;
            replay_output(&output)?;
            return Ok(AutofixOutcome::CompilationFailed);
        }
    }

    unreachable!()
}

fn run_child(args: &[String], plan_path: &Path, overlay_path: Option<&Path>) -> io::Result<Output> {
    let mut command = Command::new(env::current_exe()?);
    command
        .args(args)
        .env(AUTOFIX_CHILD_ENV, "1")
        .env(AUTOFIX_PLAN_ENV, plan_path)
        .env_remove(AUTOFIX_OVERLAY_ENV);
    if let Some(overlay_path) = overlay_path {
        command.env(AUTOFIX_OVERLAY_ENV, overlay_path);
    }
    command.output()
}

fn read_plan(path: &Path) -> io::Result<Option<SerializedBorrowckRepairPlan>> {
    match File::open(path) {
        Ok(file) => serde_json::from_reader(file).map(Some).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn plan_source_texts(plan: &SerializedBorrowckRepairPlan) -> io::Result<BTreeMap<PathBuf, &str>> {
    plan.sources
        .iter()
        .map(|source| {
            source.source.as_deref().map(|text| (source.path.clone(), text)).ok_or_else(|| {
                io::Error::other(format!(
                    "source text for {} was not included in the repair plan",
                    source.path.display()
                ))
            })
        })
        .collect()
}

fn write_overlay(
    work_dir: &Path,
    round: usize,
    overlay: &BTreeMap<PathBuf, PathBuf>,
) -> io::Result<Option<PathBuf>> {
    if overlay.is_empty() {
        return Ok(None);
    }
    let path = work_dir.join(format!("overlay-{round}.json"));
    let serialized: BTreeMap<_, _> = overlay
        .iter()
        .map(|(original, patched)| {
            Ok((path_to_utf8(original)?.to_owned(), path_to_utf8(patched)?.to_owned()))
        })
        .collect::<io::Result<_>>()?;
    serde_json::to_writer_pretty(File::create(&path)?, &serialized).map_err(io::Error::other)?;
    Ok(Some(path))
}

fn apply_plan(
    plan: &SerializedBorrowckRepairPlan,
    crate_dir: &Path,
    round: usize,
    overlay: &mut BTreeMap<PathBuf, PathBuf>,
    applied_repairs: &mut Vec<Value>,
    skipped_repairs: &mut Vec<Value>,
) -> io::Result<usize> {
    let sources = plan_source_texts(plan)?;
    let mut accepted: Vec<SerializedBorrowckRepairEdit> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut applied_groups = Vec::new();

    for repair in &plan.repairs {
        let mut candidate = Vec::new();
        let mut reason = None;
        for edit in &repair.edits {
            let Some(source) = sources.get(&edit.path) else {
                reason = Some("repair source was not included in the compiler plan");
                break;
            };
            if edit.byte_start > edit.byte_end
                || edit.byte_end > source.len()
                || !source.is_char_boundary(edit.byte_start)
                || !source.is_char_boundary(edit.byte_end)
            {
                reason = Some("repair did not describe valid UTF-8 byte boundaries");
                break;
            }
            if accepted.iter().any(|accepted| edits_conflict(accepted, edit))
                || candidate.iter().any(|accepted| edits_conflict(accepted, edit))
            {
                reason = Some("repair overlaps another borrow-checker repair");
                break;
            }
            let key = (edit.path.clone(), edit.byte_start, edit.byte_end, edit.replacement.clone());
            if !seen.contains(&key) {
                candidate.push(edit.clone());
            }
        }

        if let Some(reason) = reason {
            skipped_repairs.push(json!({
                "round": round,
                "reason": reason,
                "repair": repair,
            }));
            continue;
        }
        if candidate.is_empty() {
            continue;
        }
        for edit in &candidate {
            seen.insert((
                edit.path.clone(),
                edit.byte_start,
                edit.byte_end,
                edit.replacement.clone(),
            ));
        }
        accepted.extend(candidate);
        applied_groups.push(repair);
    }

    if accepted.is_empty() {
        return Ok(0);
    }

    let mut edits_by_path: BTreeMap<PathBuf, Vec<_>> = BTreeMap::new();
    for edit in accepted {
        edits_by_path.entry(edit.path.clone()).or_default().push(edit);
    }
    for (original, mut edits) in edits_by_path {
        let Some(source) = sources.get(&original) else {
            continue;
        };
        edits.sort_by_key(|edit| (edit.byte_start, edit.byte_end));
        let mut patched = source.to_string();
        for edit in edits.into_iter().rev() {
            patched.replace_range(edit.byte_start..edit.byte_end, &edit.replacement);
        }
        let patched_path = patched_source_path(crate_dir, &original)?;
        if let Some(parent) = patched_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&patched_path, patched)?;
        overlay.insert(original, patched_path);
    }

    applied_repairs.extend(applied_groups.into_iter().map(|repair| {
        json!({
            "round": round,
            "repair": repair,
        })
    }));
    Ok(applied_repairs.iter().filter(|repair| repair["round"] == round).count())
}

fn edits_conflict(
    left: &SerializedBorrowckRepairEdit,
    right: &SerializedBorrowckRepairEdit,
) -> bool {
    if left.path != right.path {
        return false;
    }
    if left.byte_start == right.byte_start
        && left.byte_end == right.byte_end
        && left.replacement == right.replacement
    {
        return false;
    }
    if left.byte_start == left.byte_end {
        return right.byte_start <= left.byte_start && left.byte_start <= right.byte_end;
    }
    if right.byte_start == right.byte_end {
        return left.byte_start <= right.byte_start && right.byte_start <= left.byte_end;
    }
    left.byte_start < right.byte_end && right.byte_start < left.byte_end
}

fn patched_source_path(crate_dir: &Path, original: &Path) -> io::Result<PathBuf> {
    let workspace = env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?)
        .canonicalize()
        .unwrap_or(env::current_dir()?);
    if let Ok(relative) = original.strip_prefix(workspace) {
        return Ok(crate_dir.join(relative));
    }
    let basename = original.file_name().unwrap_or_else(|| std::ffi::OsStr::new("source.rs"));
    Ok(crate_dir.join("_external").join(stable_path_hash(original)).join(basename))
}

fn crate_output_dir(output_root: &Path, plan: &SerializedBorrowckRepairPlan) -> PathBuf {
    let crate_name: String = plan
        .crate_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    output_root.join(format!("{crate_name}-{:016x}", plan.stable_crate_id))
}

fn compile_wrapper_variants(
    args: &[String],
    plan: Option<&SerializedBorrowckRepairPlan>,
    crate_dir: &Path,
) -> io::Result<Vec<Value>> {
    let Some(plan) = plan else { return Ok(Vec::new()) };
    let variants_root = crate_dir.join("variants");
    fs::create_dir_all(&variants_root)?;
    let mut results = Vec::new();

    for variant in &plan.wrapper_variants {
        let name = format!(
            "{}-{}-{}",
            safe_component(&variant.binding.name),
            variant.binding.byte_start,
            strategy_name(variant.strategy),
        );
        results.push(compile_wrapper_variant_set(
            args,
            plan,
            &variants_root,
            &name,
            std::slice::from_ref(variant),
        )?);
    }
    if plan.wrapper_variants.len() > 1 {
        results.push(compile_wrapper_variant_set(
            args,
            plan,
            &variants_root,
            "all-wrappers",
            &plan.wrapper_variants,
        )?);
    }
    Ok(results)
}

fn compile_wrapper_variant_set(
    args: &[String],
    plan: &SerializedBorrowckRepairPlan,
    variants_root: &Path,
    name: &str,
    variants: &[SerializedBorrowckWrapperVariant],
) -> io::Result<Value> {
    let variant_dir = variants_root.join(name);
    if variant_dir.exists() {
        fs::remove_dir_all(&variant_dir)?;
    }
    let sources_dir = variant_dir.join("sources");
    let artifacts_dir = variant_dir.join("artifacts");
    fs::create_dir_all(&sources_dir)?;
    fs::create_dir_all(&artifacts_dir)?;

    let bindings: Vec<_> = variants.iter().map(|variant| variant.binding.clone()).collect();
    let mut edits: Vec<_> = variants.iter().flat_map(|variant| variant.edits.clone()).collect();
    for repair in &plan.repairs {
        if repair
            .binding
            .as_ref()
            .is_some_and(|binding| bindings.iter().any(|wrapper| same_binding(binding, wrapper)))
        {
            continue;
        }
        edits.extend(repair.edits.clone());
    }

    let mut overlay = match materialize_variant_sources(plan, &sources_dir, edits) {
        Ok(overlay) => overlay,
        Err(error) => {
            let result = json!({
                "name": name,
                "bindings": bindings,
                "strategies": variants.iter().map(|variant| strategy_name(variant.strategy)).collect::<Vec<_>>(),
                "status": "rejected",
                "reason": error.to_string(),
                "directory": path_to_utf8(&variant_dir)?,
            });
            serde_json::to_writer_pretty(File::create(variant_dir.join("variant.json"))?, &result)
                .map_err(io::Error::other)?;
            return Ok(result);
        }
    };

    let child_args = redirect_variant_outputs(args, &artifacts_dir)?;
    let mut status = "failed";
    let mut repair_rounds = 0;
    let mut rejection = None;
    for round in 0..=MAX_REPAIR_ROUNDS {
        let plan_path = variant_dir.join(format!("compile-round-{round}.json"));
        let overlay_path = write_overlay(&variant_dir, round, &overlay)?;
        let output = run_child(&child_args, &plan_path, overlay_path.as_deref())?;
        fs::write(variant_dir.join("stdout.txt"), &output.stdout)?;
        fs::write(variant_dir.join("stderr.txt"), &output.stderr)?;
        repair_rounds = round;
        if output.status.success() {
            status = "success";
            break;
        }
        let Some(next_plan) = read_plan(&plan_path)? else {
            rejection = Some("variant compiler did not produce a repair plan".to_string());
            break;
        };
        if round == MAX_REPAIR_ROUNDS || next_plan.repairs.is_empty() {
            break;
        }
        let retry_dir = variant_dir.join(format!("retry-{}", round + 1));
        let mut applied = Vec::new();
        let mut skipped = Vec::new();
        if apply_plan(&next_plan, &retry_dir, round + 1, &mut overlay, &mut applied, &mut skipped)?
            == 0
        {
            rejection = Some("variant produced no additional non-overlapping repair".to_string());
            break;
        }
    }

    let result = json!({
        "name": name,
        "bindings": bindings,
        "strategies": variants.iter().map(|variant| strategy_name(variant.strategy)).collect::<Vec<_>>(),
        "status": status,
        "repair_rounds": repair_rounds,
        "reason": rejection,
        "directory": path_to_utf8(&variant_dir)?,
        "artifacts": path_to_utf8(&artifacts_dir)?,
        "stdout": path_to_utf8(&variant_dir.join("stdout.txt"))?,
        "stderr": path_to_utf8(&variant_dir.join("stderr.txt"))?,
    });
    serde_json::to_writer_pretty(File::create(variant_dir.join("variant.json"))?, &result)
        .map_err(io::Error::other)?;
    Ok(result)
}

fn materialize_variant_sources(
    plan: &SerializedBorrowckRepairPlan,
    sources_dir: &Path,
    edits: Vec<SerializedBorrowckRepairEdit>,
) -> io::Result<BTreeMap<PathBuf, PathBuf>> {
    let accepted = normalize_variant_edits(plan, edits)?;
    let sources = plan_source_texts(plan)?;
    let mut edits_by_path: BTreeMap<PathBuf, Vec<_>> = BTreeMap::new();
    for edit in accepted {
        edits_by_path.entry(edit.path.clone()).or_default().push(edit);
    }
    let mut overlay = BTreeMap::new();
    for (original, mut edits) in edits_by_path {
        let source = sources[&original];
        edits.sort_by_key(|edit| (edit.byte_start, edit.byte_end));
        let mut patched = source.to_string();
        for edit in edits.into_iter().rev() {
            patched.replace_range(edit.byte_start..edit.byte_end, &edit.replacement);
        }
        let patched_path = patched_source_path(sources_dir, &original)?;
        if let Some(parent) = patched_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&patched_path, patched)?;
        overlay.insert(original, patched_path);
    }
    Ok(overlay)
}

fn normalize_variant_edits(
    plan: &SerializedBorrowckRepairPlan,
    edits: Vec<SerializedBorrowckRepairEdit>,
) -> io::Result<Vec<SerializedBorrowckRepairEdit>> {
    let sources = plan_source_texts(plan)?;
    let mut accepted = Vec::new();
    let mut seen = BTreeSet::new();
    for edit in edits {
        let Some(source) = sources.get(&edit.path) else {
            return Err(io::Error::other("variant edit source was not included in the plan"));
        };
        if edit.byte_start > edit.byte_end
            || edit.byte_end > source.len()
            || !source.is_char_boundary(edit.byte_start)
            || !source.is_char_boundary(edit.byte_end)
        {
            return Err(io::Error::other("variant edit has invalid UTF-8 byte boundaries"));
        }
        let key = (edit.path.clone(), edit.byte_start, edit.byte_end, edit.replacement.clone());
        if !seen.insert(key) {
            continue;
        }
        if accepted.iter().any(|old| edits_conflict(old, &edit)) {
            return Err(io::Error::other("variant edit overlaps another selected repair"));
        }
        accepted.push(edit);
    }
    Ok(accepted)
}

fn redirect_variant_outputs(args: &[String], artifacts: &Path) -> io::Result<Vec<String>> {
    let artifact_output = path_to_utf8(&artifacts.join("output"))?.to_string();
    let artifact_dir = path_to_utf8(artifacts)?.to_string();
    let mut redirected = Vec::with_capacity(args.len() + 2);
    let mut has_output_dir = false;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "-o" => {
                redirected.push(arg.clone());
                redirected.push(artifact_output.clone());
                has_output_dir = true;
                index += 2;
            }
            "--out-dir" => {
                redirected.push(arg.clone());
                redirected.push(artifact_dir.clone());
                has_output_dir = true;
                index += 2;
            }
            "--emit" if index + 1 < args.len() => {
                redirected.push(arg.clone());
                redirected.push(strip_emit_paths(&args[index + 1]));
                index += 2;
            }
            "-C" if index + 1 < args.len() && args[index + 1].starts_with("incremental=") => {
                index += 2;
            }
            _ if arg.starts_with("--out-dir=") => {
                redirected.push(format!("--out-dir={artifact_dir}"));
                has_output_dir = true;
                index += 1;
            }
            _ if arg.starts_with("--emit=") => {
                redirected.push(format!("--emit={}", strip_emit_paths(&arg[7..])));
                index += 1;
            }
            _ if arg.starts_with("-Cincremental=") => {
                index += 1;
            }
            _ if arg.starts_with("-o") && arg.len() > 2 => {
                redirected.push(format!("-o{artifact_output}"));
                has_output_dir = true;
                index += 1;
            }
            _ => {
                redirected.push(arg.clone());
                index += 1;
            }
        }
    }
    if !has_output_dir {
        redirected.push("--out-dir".into());
        redirected.push(artifact_dir);
    }
    Ok(redirected)
}

fn strip_emit_paths(emit: &str) -> String {
    emit.split(',')
        .map(|kind| kind.split_once('=').map_or(kind, |(kind, _)| kind))
        .collect::<Vec<_>>()
        .join(",")
}

fn same_binding(left: &SerializedBorrowckBinding, right: &SerializedBorrowckBinding) -> bool {
    left.path == right.path && left.byte_start == right.byte_start
}

fn strategy_name(strategy: BorrowckWrapperStrategy) -> &'static str {
    match strategy {
        BorrowckWrapperStrategy::Arc => "arc",
        BorrowckWrapperStrategy::ArcMutex => "arc_mutex",
        BorrowckWrapperStrategy::ArcRwLock => "arc_rw_lock",
        BorrowckWrapperStrategy::Mutex => "mutex",
        BorrowckWrapperStrategy::Rc => "rc",
        BorrowckWrapperStrategy::RefCell => "ref_cell",
        BorrowckWrapperStrategy::RcRefCell => "rc_ref_cell",
        BorrowckWrapperStrategy::RwLock => "rw_lock",
    }
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn write_repair_manifest(
    crate_dir: &Path,
    plan: &SerializedBorrowckRepairPlan,
    status: &str,
    rounds: usize,
    overlay: &BTreeMap<PathBuf, PathBuf>,
    applied_repairs: &[Value],
    skipped_repairs: &[Value],
    wrapper_plan: Option<&SerializedBorrowckRepairPlan>,
    wrapper_results: &[Value],
) -> io::Result<()> {
    fs::create_dir_all(crate_dir)?;
    let files: Vec<_> = overlay
        .iter()
        .map(|(original, patched)| {
            Ok(json!({
                "original": path_to_utf8(original)?,
                "patched": path_to_utf8(patched)?,
            }))
        })
        .collect::<io::Result<_>>()?;
    let manifest = json!({
        "format_version": 2,
        "compiler_revision": option_env!("CFG_VER_HASH").unwrap_or("unknown"),
        "crate_name": plan.crate_name,
        "stable_crate_id": format!("{:016x}", plan.stable_crate_id),
        "status": status,
        "repair_rounds": rounds,
        "files": files,
        "applied_repairs": applied_repairs,
        "skipped_repairs": skipped_repairs,
        "wrapper_variants": wrapper_results,
        "wrapper_rejections": wrapper_plan.map_or(&[][..], |plan| &plan.wrapper_rejections),
    });
    serde_json::to_writer_pretty(File::create(crate_dir.join("repair.json"))?, &manifest)
        .map_err(io::Error::other)
}

fn replay_output(output: &Output) -> io::Result<()> {
    io::stdout().write_all(&output.stdout)?;
    io::stderr().write_all(&output.stderr)
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() { Ok(path.to_path_buf()) } else { Ok(env::current_dir()?.join(path)) }
}

fn path_to_utf8(path: &Path) -> io::Result<&str> {
    path.to_str().ok_or_else(|| io::Error::other("borrowck autofix requires UTF-8 paths"))
}

fn stable_path_hash(path: &Path) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

struct OverlayFileLoader {
    overlay: BTreeMap<PathBuf, PathBuf>,
}

struct WorkDir(PathBuf);

impl WorkDir {
    fn create() -> io::Result<Self> {
        let nonce =
            SystemTime::now().duration_since(UNIX_EPOCH).map_err(io::Error::other)?.as_nanos();
        let path =
            env::temp_dir().join(format!("rustc-borrowck-autofix-{}-{nonce}", std::process::id()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl OverlayFileLoader {
    fn resolve<'a>(&'a self, path: &'a Path) -> &'a Path {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
        };
        let canonical = absolute.canonicalize().unwrap_or(absolute);
        self.overlay.get(&canonical).map_or(path, PathBuf::as_path)
    }
}

impl FileLoader for OverlayFileLoader {
    fn file_exists(&self, path: &Path) -> bool {
        RealFileLoader.file_exists(self.resolve(path))
    }

    fn read_file(&self, path: &Path) -> io::Result<String> {
        RealFileLoader.read_file(self.resolve(path))
    }

    fn read_binary_file(&self, path: &Path) -> io::Result<Arc<[u8]>> {
        RealFileLoader.read_binary_file(self.resolve(path))
    }

    fn current_directory(&self) -> io::Result<PathBuf> {
        RealFileLoader.current_directory()
    }
}
