//@ ignore-cross-compile
// Reason: the repaired binary is executed.

use std::fs;
use std::path::{Path, PathBuf};

use run_make_support::{rfs, run, rustc};

fn main() {
    let original = rfs::read_to_string("input.rs");

    rustc()
        .input("input.rs")
        .run_fail()
        .assert_stderr_contains("consider `RefCell`")
        .assert_stderr_contains("consider changing the surrounding APIs to use `Rc`");

    rfs::create_dir("repairs");
    rustc().input("input.rs").output("fixed").arg("-Zborrowck-autofix=repairs").run();
    run("fixed");

    assert_eq!(rfs::read_to_string("input.rs"), original);

    let patched_sources = find_files(Path::new("repairs"), "rs");
    assert_eq!(patched_sources.len(), 1, "unexpected patched sources: {patched_sources:?}");
    let patched = rfs::read_to_string(&patched_sources[0]);
    assert!(patched.contains("let mut values"), "{patched}");
    assert!(patched.contains("consume(value.clone())"), "{patched}");

    let manifests = find_files(Path::new("repairs"), "json");
    assert_eq!(manifests.len(), 1, "unexpected manifests: {manifests:?}");
    let manifest = rfs::read_to_string(&manifests[0]);
    assert!(manifest.contains(r#""status": "success""#), "{manifest}");
    assert!(manifest.contains("make_binding_mutable"), "{manifest}");
    assert!(manifest.contains("clone_moved_value"), "{manifest}");

    rustc()
        .input("input.rs")
        .arg("-Zborrowck-autofix-wrapper-variants")
        .run_fail()
        .assert_stderr_contains("requires -Zborrowck-autofix=<PATH>");

    rustc()
        .input("input.rs")
        .arg("-Zborrowck-wrapper-suggestions")
        .arg("-Zborrowck-autofix=suggestions-must-not-write")
        .run_fail()
        .assert_stderr_contains("cannot be combined with -Zborrowck-autofix");

    test_editor_suggestions(
        "box-rc.rs",
        &[
            ("borrowck_wrapper_rc", "Use Rc for shared ownership (compiler validated)"),
            (
                "borrowck_wrapper_arc",
                "Use Arc for thread-safe shared ownership (compiler validated)",
            ),
        ],
    );
    test_ownership_events();
    test_ownership_topology();
    test_editor_suggestions(
        "ref-cell.rs",
        &[
            (
                "borrowck_wrapper_ref_cell",
                "Use RefCell for interior mutability (compiler validated)",
            ),
            (
                "borrowck_wrapper_mutex",
                "Use Mutex for synchronized interior mutability (compiler validated)",
            ),
            (
                "borrowck_wrapper_rw_lock",
                "Use RwLock for synchronized interior mutability (compiler validated)",
            ),
        ],
    );
    test_editor_suggestions(
        "box-rc-ref-cell.rs",
        &[
            (
                "borrowck_wrapper_rc_ref_cell",
                "Use Rc<RefCell<_>> for shared mutable ownership (compiler validated)",
            ),
            (
                "borrowck_wrapper_arc_mutex",
                "Use Arc<Mutex<_>> for thread-safe shared mutation (compiler validated)",
            ),
            (
                "borrowck_wrapper_arc_rw_lock",
                "Use Arc<RwLock<_>> for read-heavy shared mutation (compiler validated)",
            ),
        ],
    );

    test_single_wrapper("box-rc.rs", "rc", false);
    test_single_wrapper("ref-cell.rs", "ref_cell", true);
    test_single_wrapper("box-rc-ref-cell.rs", "rc_ref_cell", true);
    test_single_wrapper("rc-ref-cell.rs", "rc_ref_cell", false);

    let wrapper_original = rfs::read_to_string("wrapper-cases.rs");
    rfs::create_dir("wrapper-repairs");
    rustc()
        .input("wrapper-cases.rs")
        .output("minimal-wrapper-output")
        .arg("-Zborrowck-autofix=wrapper-repairs")
        .arg("-Zborrowck-autofix-wrapper-variants")
        .run_fail();
    assert_eq!(rfs::read_to_string("wrapper-cases.rs"), wrapper_original);

    let wrapper_manifest = find_named_file(Path::new("wrapper-repairs"), "repair.json");
    let wrapper_manifest = rfs::read_to_string(wrapper_manifest);
    assert!(wrapper_manifest.contains(r#""format_version": 2"#), "{wrapper_manifest}");
    assert!(wrapper_manifest.contains(r#""status": "success""#), "{wrapper_manifest}");
    assert!(wrapper_manifest.contains(r#""strategies": ["#), "{wrapper_manifest}");
    assert!(wrapper_manifest.contains(r#""rc""#), "{wrapper_manifest}");
    assert!(wrapper_manifest.contains(r#""ref_cell""#), "{wrapper_manifest}");
    assert!(wrapper_manifest.contains(r#""rc_ref_cell""#), "{wrapper_manifest}");

    let all_wrappers =
        find_file_below(Path::new("wrapper-repairs"), Path::new("all-wrappers/artifacts/output"));
    run(all_wrappers.to_str().unwrap());
    let generated = rfs::read_to_string(find_file_below(
        Path::new("wrapper-repairs"),
        Path::new("all-wrappers/sources/wrapper-cases.rs"),
    ));
    assert!(generated.contains("::std::rc::Rc<Vec<i32>>"), "{generated}");
    assert!(generated.contains("::std::cell::RefCell<Vec<i32>>"), "{generated}");
    assert!(generated.contains("::std::rc::Rc<::std::cell::RefCell<Vec<i32>>>"), "{generated}");
    assert!(generated.contains("values.clone()"), "{generated}");
    assert!(generated.contains("values.borrow_mut().push"), "{generated}");
    assert!(generated.contains("values.borrow().len()"), "{generated}");

    rfs::create_dir("rejected-repairs");
    rustc()
        .input("wrapper-rejected.rs")
        .output("rejected-minimal-output")
        .arg("-Zborrowck-autofix=rejected-repairs")
        .arg("-Zborrowck-autofix-wrapper-variants")
        .run();
    run("rejected-minimal-output");
    let rejected_manifest = find_named_file(Path::new("rejected-repairs"), "repair.json");
    let rejected_manifest = rfs::read_to_string(rejected_manifest);
    assert!(rejected_manifest.contains("unsupported consuming"), "{rejected_manifest}");
    assert!(rejected_manifest.contains(r#""wrapper_variants": []"#), "{rejected_manifest}");
}

fn test_ownership_topology() {
    rfs::create_dir("ownership-model-topology");
    rustc()
        .input("ownership-model-topology.rs")
        .output("ownership-model-topology-bin")
        .arg("-Zborrowck-ownership-model=ownership-model-topology")
        .run();
    run("ownership-model-topology-bin");
    let models = find_files(Path::new("ownership-model-topology"), "json");
    assert_eq!(models.len(), 1, "unexpected ownership models: {models:?}");
    let model = rfs::read_to_string(&models[0]);
    for fact in [
        r#""schema_version":6"#,
        r#""target_triple":"x86_64-unknown-linux-gnu""#,
        r#""kind":"box_allocation""#,
        r#""kind":"rc_allocation""#,
        r#""kind":"arc_allocation""#,
        r#""kind":"weak_allocation""#,
        r#""kind":"pin_constraint""#,
        r#""kind":"cell_state""#,
        r#""kind":"ref_cell_state""#,
        r#""kind":"unsafe_cell_state""#,
        r#""kind":"once_state""#,
        r#""kind":"mutex_state""#,
        r#""kind":"rw_lock_state""#,
        r#""kind":"container_header""#,
        r#""kind":"container_buffer""#,
        r#""kind":"conditional_value""#,
        r#""kind":"reference_handle""#,
        r#""kind":"fat_pointer_metadata""#,
        r#""kind":"raw_pointer""#,
        r#""relation":"weak_reference""#,
        r#""kind":"raw_pointer_deref""#,
        r#""kind":"weak_upgrade""#,
        r#""kind":"lock_acquire""#,
        r#""kind":"wrapper_borrow_mut""#,
        r#""kind":"option_extract""#,
        r#""kind":"initialize""#,
    ] {
        assert!(model.contains(fact), "missing {fact}: {model}");
    }
    assert!(
        model.contains("runtime length/capacity/node count is unknown"),
        "container topology fabricated runtime data: {model}"
    );
    assert!(
        model.contains("optimized machine placement may differ"),
        "source storage must not claim optimized physical placement: {model}"
    );
    // `source` is intentionally used as the source-node ID on graph edges.  The
    // persistent artifact must not, however, embed a copy of the source file.
    for source_text_field in
        [r#""source_text":"#, r#""sourceText":"#, r#""source_content":"#, r#""sourceContent":"#]
    {
        assert!(
            !model.contains(source_text_field),
            "persistent ownership artifacts must not duplicate source text in {source_text_field}: {model}"
        );
    }

    let stable_source = r#"
fn main() {
    let kept: Box<Vec<i32>> = Box::new(vec![1, 2, 3]);
    println!("{}", kept.len());
}
"#;
    rfs::write("stable-ownership-ids.rs", stable_source);
    rfs::create_dir("stable-ownership-model-a");
    rustc()
        .input("stable-ownership-ids.rs")
        .crate_name("stable_ownership_ids")
        .output("stable-ownership-ids-a")
        .arg("-Zborrowck-ownership-model=stable-ownership-model-a")
        .run();
    let first = rfs::read_to_string(
        find_files(Path::new("stable-ownership-model-a"), "json")
            .first()
            .expect("first stable-id model"),
    );

    rfs::write(
        "stable-ownership-ids.rs",
        stable_source.replace(
            "fn main() {",
            "fn main() {\n    let unrelated: String = String::from(\"unrelated\");\n    drop(unrelated);",
        ),
    );
    rfs::create_dir("stable-ownership-model-b");
    rustc()
        .input("stable-ownership-ids.rs")
        .crate_name("stable_ownership_ids")
        .output("stable-ownership-ids-b")
        .arg("-Zborrowck-ownership-model=stable-ownership-model-b")
        .run();
    let second = rfs::read_to_string(
        find_files(Path::new("stable-ownership-model-b"), "json")
            .first()
            .expect("second stable-id model"),
    );
    assert_eq!(
        graph_node_id_for_place(&first, "kept"),
        graph_node_id_for_place(&second, "kept"),
        "inserting an unrelated binding must not change the stable graph ID"
    );
}

fn graph_node_id_for_place(model: &str, place: &str) -> String {
    let marker = format!(r#""place":"{place}","kind":"binding""#);
    let marker_start = model.find(&marker).unwrap_or_else(|| panic!("missing {marker}: {model}"));
    let before = &model[..marker_start];
    let id_start = before
        .rfind(r#""id":"#)
        .unwrap_or_else(|| panic!("missing node id before {marker}: {model}"))
        + r#""id":"#.len();
    let id_end =
        model[id_start..].find('"').map(|offset| id_start + offset).expect("closing node id quote");
    model[id_start..id_end].to_owned()
}

fn test_ownership_events() {
    let original = rfs::read_to_string("ownership-events.rs");
    let normal = rustc().input("ownership-events.rs").arg("--error-format=json").run();
    assert!(!normal.stderr_utf8().contains("borrowck_ownership_"));
    let output = rustc()
        .input("ownership-events.rs")
        .arg("-Zborrowck-ownership-events")
        .arg("--error-format=json")
        .run();
    assert_eq!(rfs::read_to_string("ownership-events.rs"), original);
    let stderr = output.stderr_utf8();
    for code in [
        "borrowck_ownership_move",
        "borrowck_ownership_partial_move",
        "borrowck_ownership_borrow_shared",
        "borrowck_ownership_borrow_mutable",
        "borrowck_ownership_copy",
        "borrowck_ownership_borrow_end",
        "borrowck_ownership_reinitialize",
        "borrowck_ownership_last_use",
        "borrowck_ownership_drop",
    ] {
        assert!(stderr.contains(code), "missing {code}: {stderr}");
    }
    assert!(stderr.contains(r#"\"version\":1"#), "{stderr}");
    assert!(stderr.contains(r#"\"source_hash\":"#), "{stderr}");

    rfs::create_dir("ownership-model");
    let output = rustc()
        .input("ownership-events.rs")
        .arg("-Zborrowck-ownership-model=ownership-model")
        .arg("--error-format=json")
        .run();
    let stderr = output.stderr_utf8();
    assert!(
        !stderr.contains("borrowck_ownership_move"),
        "the file artifact is the model transport; direct model mode must not duplicate event diagnostics: {stderr}"
    );
    assert!(!stderr.contains("borrowck_ownership_move"), "{stderr}");
    let models = find_files(Path::new("ownership-model"), "json");
    assert_eq!(models.len(), 1, "unexpected ownership models: {models:?}");
    assert!(
        !Path::new("ownership-model/.validation-cache").exists(),
        "model-only compilation must not validate or cache wrapper rewrites"
    );
    let model = rfs::read_to_string(&models[0]);
    for field in [
        r#""schema_version":6"#,
        r#""ownership_bodies":"#,
        r#""ownership_bindings":"#,
        r#""ownership_loans":"#,
        r#""memory_layers":"#,
        r#""successors":"#,
        r#""live_points":"#,
        r#""end_points":"#,
        r#""event_id":"#,
        r#""body_id":"#,
        r#""basic_block":"#,
        r#""statement_index":"#,
        r#""state":"#,
        r#""place":"#,
        r#""loan_id":"#,
        r#""destination":"#,
        r#""memory_graph":"#,
        r#""nodes":"#,
        r#""edges":"#,
        r#""snapshots":"#,
        r#""access_paths":"#,
        r#""physical_placement_note":"#,
    ] {
        assert!(model.contains(field), "missing {field}: {model}");
    }
    assert!(
        model.contains(r#""kind":"local_binding""#) && model.contains(r#""label":"moved""#),
        "move destination was not serialized as the receiving binding: {model}"
    );
    assert!(
        model.contains(r#""kind":"function_argument""#),
        "call-argument moves should preserve their destination role: {model}"
    );
    assert!(
        model.contains(r#""relation":"moved_to""#),
        "a move between bindings should produce a stable graph edge: {model}"
    );
    assert!(
        model.contains(r#""kind":"move""#) && model.contains(r#""deltas":["#),
        "ownership events should produce bounded state snapshots: {model}"
    );

    // Successful crates emit models too. In particular, formatting nested standard-library types
    // must not touch the diagnostic-only trimmed-path cache and panic at session teardown.
    rfs::create_dir("ownership-model-success");
    rustc()
        .input("ownership-model-success.rs")
        .output("ownership-model-success-bin")
        .arg("-Zborrowck-ownership-model=ownership-model-success")
        .run();
    let success_models = find_files(Path::new("ownership-model-success"), "json");
    assert_eq!(success_models.len(), 1, "unexpected ownership models: {success_models:?}");
    let success_model = rfs::read_to_string(&success_models[0]);
    for field in [r#""kind":"rc_allocation""#, r#""kind":"ref_cell_state""#] {
        assert!(success_model.contains(field), "missing {field}: {success_model}");
    }
    assert!(success_model.contains(r#""kind":"clone""#), "{success_model}");
    assert_eq!(
        success_model.matches(r#""kind":"control_block""#).count(),
        1,
        "Rc::clone must produce two handles sharing one control block: {success_model}"
    );
}

fn test_editor_suggestions(input: &str, suggestions: &[(&str, &str)]) {
    let original = rfs::read_to_string(input);
    let output = rustc()
        .input(input)
        .arg("-Zborrowck-wrapper-suggestions")
        .arg("--error-format=json")
        .run_fail();
    assert_eq!(rfs::read_to_string(input), original);
    let stderr = output.stderr_utf8();
    for (code, title) in suggestions {
        assert!(stderr.contains(code), "missing {code}: {stderr}");
        assert!(stderr.contains(title), "missing {title}: {stderr}");
    }
    assert!(stderr.contains(r#""label":"ownership error occurs here""#), "{stderr}");
    assert!(stderr.contains(r#""suggestion_applicability":"MaybeIncorrect""#), "{stderr}");
}

fn test_single_wrapper(input: &str, strategy: &str, minimal_succeeds: bool) {
    let stem = input.strip_suffix(".rs").unwrap();
    let repairs = format!("{stem}-repairs");
    let output = format!("{stem}-minimal-output");
    rfs::create_dir(&repairs);
    let mut command = rustc();
    command
        .input(input)
        .output(&output)
        .arg(format!("-Zborrowck-autofix={repairs}"))
        .arg("-Zborrowck-autofix-wrapper-variants");
    if minimal_succeeds {
        command.run();
        run(&output);
    } else {
        command.run_fail();
    }

    let variant_manifest = find_named_file(Path::new(&repairs), "variant.json");
    let variant_manifest = rfs::read_to_string(variant_manifest);
    assert!(variant_manifest.contains(r#""status": "success""#), "{variant_manifest}");
    assert!(variant_manifest.contains(&format!(r#""{strategy}""#)), "{variant_manifest}");

    let artifact = find_file_below(Path::new(&repairs), Path::new("artifacts/output"));
    run(artifact.to_str().unwrap());
}

fn find_files(root: &Path, extension: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(find_files(&path, extension));
        } else if path.extension().is_some_and(|candidate| candidate == extension) {
            files.push(path);
        }
    }
    files
}

fn find_named_file(root: &Path, name: &str) -> PathBuf {
    let matches = find_files(root, Path::new(name).extension().unwrap().to_str().unwrap());
    let matches: Vec<_> = matches
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|candidate| candidate == name))
        .collect();
    assert_eq!(matches.len(), 1, "expected one {name}: {matches:?}");
    matches.into_iter().next().unwrap()
}

fn find_file_below(root: &Path, suffix: &Path) -> PathBuf {
    let mut matches = Vec::new();
    find_matching_files(root, suffix, &mut matches);
    assert_eq!(matches.len(), 1, "expected one {suffix:?}: {matches:?}");
    matches.into_iter().next().unwrap()
}

fn find_matching_files(root: &Path, suffix: &Path, matches: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            find_matching_files(&path, suffix, matches);
        } else if path.ends_with(suffix) {
            matches.push(path);
        }
    }
}
