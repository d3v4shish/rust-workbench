//! This module is responsible for implementing handlers for Language Server
//! Protocol. This module specifically handles requests.

use std::{fs, io::Write as _, ops::Not, process::Stdio};

use anyhow::Context;

use base64::{Engine, prelude::BASE64_STANDARD};
use ide::{
    AssistKind, AssistResolveStrategy, Cancellable, CompletionFieldsToResolve,
    CompletionItemImport, FilePosition, FileRange, FileStructureConfig, FindAllRefsConfig,
    HoverAction, HoverGotoTypeData, InlayFieldsToResolve, Query, RangeInfo, Runnable, RunnableKind,
    SingleResolve, SourceChange, TextEdit,
};
use ide_db::{FxHashMap, FxHashSet, SymbolKind};
use itertools::Itertools;
use lsp_server::ErrorCode;
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    CodeLens, CompletionItem, Contents, DocumentChange, FoldingRange, FoldingRangeParams,
    InlayHint, InlayHintParams, Location, LocationLink, Position, PrepareRenameResult, Range,
    RenameParams, ResourceOperationKind, SemanticTokens, SemanticTokensDeltaParams,
    SemanticTokensDeltaResponse, SemanticTokensParams, SemanticTokensRangeParams,
    SymbolInformation, SymbolTag, TextDocumentIdentifier, Uri, WorkspaceEdit,
};
use paths::Utf8PathBuf;
use project_model::{CargoWorkspace, ManifestPath, ProjectWorkspaceKind, TargetKind};
use serde_json::json;
use stdx::{format_to, never};
use syntax::{
    TextRange, TextSize,
    ast::{self, AstNode, HasName},
};
use triomphe::Arc;
use vfs::{AbsPath, AbsPathBuf, FileId, VfsPath};

use crate::{
    config::{
        ClientCommandsConfig, Config, HoverActionsConfig, RustfmtConfig, WorkspaceSymbolConfig,
    },
    diagnostics::{
        Fix, OwnershipEvent, OwnershipEventKind, OwnershipState, OwnershipWrapperFix,
        convert_diagnostic, ownership_diagnostics_for_file, ownership_events_for_file,
        stable_source_hash,
    },
    global_state::{FetchWorkspaceRequest, GlobalState, GlobalStateSnapshot},
    line_index::{LineEndings, LineIndex},
    lsp::{
        LspError, completion_item_hash,
        ext::{
            GetFailedObligationsParams, InternalTestingFetchConfigOption,
            InternalTestingFetchConfigParams, InternalTestingFetchConfigResponse,
        },
        from_proto, to_proto,
        utils::{all_edits_are_disjoint, invalid_params_error},
    },
    lsp_ext::{
        self, CrateInfoResult, ExternalDocsPair, ExternalDocsResponse, FetchDependencyListParams,
        FetchDependencyListResult, PositionOrRange, ViewCrateGraphParams, WorkspaceSymbolParams,
    },
    target_spec::{CargoTargetSpec, TargetSpec},
    test_runner::{CargoTestHandle, TestTarget},
    try_default,
};

pub(crate) fn handle_workspace_reload(state: &mut GlobalState, _: ()) -> anyhow::Result<()> {
    state.proc_macro_clients = Arc::from_iter([]);
    state.build_deps_changed = false;

    let req = FetchWorkspaceRequest { path: None, force_crate_graph_reload: false };
    state.fetch_workspaces_queue.request_op("reload workspace request".to_owned(), req);
    Ok(())
}

pub(crate) fn handle_proc_macros_rebuild(state: &mut GlobalState, _: ()) -> anyhow::Result<()> {
    state.proc_macro_clients = Arc::from_iter([]);
    state.build_deps_changed = false;

    state.fetch_build_data_queue.request_op("rebuild proc macros request".to_owned(), ());
    Ok(())
}

pub(crate) fn handle_analyzer_status(
    snap: GlobalStateSnapshot,
    params: lsp_ext::AnalyzerStatusParams,
) -> anyhow::Result<String> {
    let _p = tracing::info_span!("handle_analyzer_status").entered();

    let mut buf = String::new();

    let mut file_id = None;
    if let Some(tdi) = params.text_document {
        match from_proto::file_id(&snap, &tdi.uri) {
            Ok(Some(it)) => file_id = Some(it),
            Ok(None) => {}
            Err(_) => format_to!(buf, "file {} not found in vfs", tdi.uri),
        }
    }

    if snap.workspaces.is_empty() {
        buf.push_str("No workspaces\n")
    } else {
        buf.push_str("Workspaces:\n");
        format_to!(
            buf,
            "Loaded {:?} packages across {} workspace{}.\n",
            snap.workspaces.iter().map(|w| w.n_packages()).sum::<usize>(),
            snap.workspaces.len(),
            if snap.workspaces.len() == 1 { "" } else { "s" }
        );

        format_to!(
            buf,
            "Workspace root folders: {:?}",
            snap.workspaces.iter().map(|ws| ws.manifest_or_root()).collect::<Vec<&AbsPath>>()
        );
    }
    buf.push_str("\nAnalysis:\n");
    buf.push_str(
        &snap
            .analysis
            .status(file_id)
            .unwrap_or_else(|_| "Analysis retrieval was cancelled".to_owned()),
    );

    buf.push_str("\nVersion: \n");
    format_to!(buf, "{}", crate::version());

    buf.push_str("\nConfiguration: \n");
    format_to!(buf, "{:#?}", snap.config);

    Ok(buf)
}

pub(crate) fn handle_memory_usage(_state: &mut GlobalState, _: ()) -> anyhow::Result<String> {
    let _p = tracing::info_span!("handle_memory_usage").entered();

    #[cfg(not(feature = "dhat"))]
    {
        Err(anyhow::anyhow!(
            "Memory profiling is not enabled for this build of rust-analyzer.\n\n\
            To build rust-analyzer with profiling support, pass `--features dhat --profile dev-rel` to `cargo build`
            when building from source, or pass `--enable-profiling` to `cargo xtask`."
        ))
    }
    #[cfg(feature = "dhat")]
    {
        if let Some(dhat_output_file) = _state.config.dhat_output_file() {
            let mut profiler = crate::DHAT_PROFILER.lock().unwrap();
            let old_profiler = profiler.take();
            // Need to drop the old profiler before creating a new one.
            drop(old_profiler);
            *profiler = Some(dhat::Profiler::builder().file_name(&dhat_output_file).build());
            Ok(format!(
                "Memory profile was saved successfully to {dhat_output_file}.\n\n\
                See https://docs.rs/dhat/latest/dhat/#viewing for how to inspect the profile."
            ))
        } else {
            Err(anyhow::anyhow!(
                "Please set `rust-analyzer.profiling.memoryProfile` to the path where you want to save the profile."
            ))
        }
    }
}

pub(crate) fn handle_view_syntax_tree(
    snap: GlobalStateSnapshot,
    params: lsp_ext::ViewSyntaxTreeParams,
) -> anyhow::Result<String> {
    let _p = tracing::info_span!("handle_view_syntax_tree").entered();
    let id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let res = snap.analysis.view_syntax_tree(id)?;
    Ok(res)
}

pub(crate) fn handle_view_hir(
    snap: GlobalStateSnapshot,
    params: lsp_types::TextDocumentPositionParams,
) -> anyhow::Result<String> {
    let _p = tracing::info_span!("handle_view_hir").entered();
    let position = try_default!(from_proto::file_position(&snap, &params)?);
    let res = snap.analysis.view_hir(position)?;
    Ok(res)
}

pub(crate) fn handle_view_mir(
    snap: GlobalStateSnapshot,
    params: lsp_types::TextDocumentPositionParams,
) -> anyhow::Result<String> {
    let _p = tracing::info_span!("handle_view_mir").entered();
    let position = try_default!(from_proto::file_position(&snap, &params)?);
    let res = snap.analysis.view_mir(position)?;
    Ok(res)
}

pub(crate) fn handle_interpret_function(
    snap: GlobalStateSnapshot,
    params: lsp_types::TextDocumentPositionParams,
) -> anyhow::Result<String> {
    let _p = tracing::info_span!("handle_interpret_function").entered();
    let position = try_default!(from_proto::file_position(&snap, &params)?);
    let res = snap.analysis.interpret_function(position)?;
    Ok(res)
}

pub(crate) fn handle_view_file_text(
    snap: GlobalStateSnapshot,
    params: lsp_types::TextDocumentIdentifier,
) -> anyhow::Result<String> {
    let file_id = try_default!(from_proto::file_id(&snap, &params.uri)?);
    Ok(snap.analysis.file_text(file_id)?.to_string())
}

pub(crate) fn handle_view_item_tree(
    snap: GlobalStateSnapshot,
    params: lsp_ext::ViewItemTreeParams,
) -> anyhow::Result<String> {
    let _p = tracing::info_span!("handle_view_item_tree").entered();
    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let res = snap.analysis.view_item_tree(file_id)?;
    Ok(res)
}

// cargo test requires:
// - the package is a member of the workspace
// - the target in the package is not a build script (custom-build)
// - the package name - the root of the test identifier supplied to this handler can be
//   a package or a target inside a package.
// - the target name - if the test identifier is a target, it's needed in addition to the
//   package name to run the right test
// - real names - the test identifier uses the namespace form where hyphens are replaced with
//   underscores. cargo test requires the real name.
// - the target kind e.g. bin or lib
fn all_test_targets(cargo: &CargoWorkspace) -> impl Iterator<Item = TestTarget> {
    cargo.packages().filter(|p| cargo[*p].is_member).flat_map(|p| {
        let package = &cargo[p];
        package.targets.iter().filter_map(|t| {
            let target = &cargo[*t];
            if target.kind == TargetKind::BuildScript {
                None
            } else {
                Some(TestTarget {
                    package: package.name.clone(),
                    target: target.name.clone(),
                    kind: target.kind,
                })
            }
        })
    })
}

fn find_test_target(namespace_root: &str, cargo: &CargoWorkspace) -> Option<TestTarget> {
    all_test_targets(cargo).find(|t| namespace_root == t.target.replace('-', "_"))
}

pub(crate) fn handle_run_test(
    state: &mut GlobalState,
    params: lsp_ext::RunTestParams,
) -> anyhow::Result<()> {
    if let Some(_session) = state.test_run_session.take() {
        state.send_notification::<lsp_ext::EndRunTestNotification>(());
    }

    let mut handles = vec![];
    for ws in &*state.workspaces {
        if let ProjectWorkspaceKind::Cargo { cargo, .. } = &ws.kind {
            // need to deduplicate `include` to avoid redundant test runs
            let tests = match params.include {
                Some(ref include) => include
                    .iter()
                    .unique()
                    .filter_map(|test| {
                        let (root, remainder) = match test.split_once("::") {
                            Some((root, remainder)) => (root.to_owned(), Some(remainder)),
                            None => (test.clone(), None),
                        };
                        if let Some(target) = find_test_target(&root, cargo) {
                            Some((target, remainder))
                        } else {
                            tracing::error!("Test target not found for: {test}");
                            None
                        }
                    })
                    .collect_vec(),
                None => all_test_targets(cargo).map(|target| (target, None)).collect(),
            };

            for (target, path) in tests {
                let handle = CargoTestHandle::new(
                    path,
                    state.config.cargo_test_options(None),
                    cargo.workspace_root(),
                    Some(cargo.target_directory().as_ref()),
                    target,
                    state.test_run_sender.clone(),
                    ws.toolchain.as_ref(),
                )?;
                handles.push(handle);
            }
        }
    }
    // Each process send finished signal twice, once for stdout and once for stderr
    state.test_run_remaining_jobs = 2 * handles.len();
    state.test_run_session = Some(handles);
    Ok(())
}

pub(crate) fn handle_discover_test(
    snap: GlobalStateSnapshot,
    params: lsp_ext::DiscoverTestParams,
) -> anyhow::Result<lsp_ext::DiscoverTestResults> {
    let _p = tracing::info_span!("handle_discover_test").entered();
    let (tests, scope) = match params.test_id {
        Some(id) => {
            let crate_id = id.split_once("::").map(|it| it.0).unwrap_or(&id);
            (
                snap.analysis.discover_tests_in_crate_by_test_id(crate_id)?,
                Some(vec![crate_id.to_owned()]),
            )
        }
        None => (snap.analysis.discover_test_roots()?, None),
    };

    Ok(lsp_ext::DiscoverTestResults {
        tests: tests
            .into_iter()
            .filter_map(|t| {
                let line_index = t.file.and_then(|f| snap.file_line_index(f).ok());
                to_proto::test_item(&snap, t, line_index.as_ref())
            })
            .collect(),
        scope,
        scope_file: None,
    })
}

pub(crate) fn handle_view_crate_graph(
    snap: GlobalStateSnapshot,
    params: ViewCrateGraphParams,
) -> anyhow::Result<String> {
    let _p = tracing::info_span!("handle_view_crate_graph").entered();
    let dot = snap.analysis.view_crate_graph(params.full)?;
    Ok(dot)
}

pub(crate) fn handle_expand_macro(
    snap: GlobalStateSnapshot,
    params: lsp_ext::ExpandMacroParams,
) -> anyhow::Result<Option<lsp_ext::ExpandedMacro>> {
    let _p = tracing::info_span!("handle_expand_macro").entered();
    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let line_index = snap.file_line_index(file_id)?;
    let offset = from_proto::offset(&line_index, params.position)?;

    let res = snap.analysis.expand_macro(FilePosition { file_id, offset })?;
    Ok(res.map(|it| lsp_ext::ExpandedMacro { name: it.name, expansion: it.expansion }))
}

pub(crate) fn handle_selection_range(
    snap: GlobalStateSnapshot,
    params: lsp_types::SelectionRangeParams,
) -> anyhow::Result<Option<Vec<lsp_types::SelectionRange>>> {
    let _p = tracing::info_span!("handle_selection_range").entered();
    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let line_index = snap.file_line_index(file_id)?;
    let res: anyhow::Result<Vec<lsp_types::SelectionRange>> = params
        .positions
        .iter()
        .map(|position| {
            let offset = from_proto::offset(&line_index, *position)?;
            let mut ranges = Vec::new();
            {
                let mut range = TextRange::new(offset, offset);
                loop {
                    ranges.push(range);
                    let frange = FileRange { file_id, range };
                    let next = snap.analysis.extend_selection(frange)?;
                    if next == range {
                        break;
                    } else {
                        range = next
                    }
                }
            }
            let mut range = lsp_types::SelectionRange {
                range: to_proto::range(&line_index, *ranges.last().unwrap()),
                parent: None,
            };
            for &r in ranges.iter().rev().skip(1) {
                range = lsp_types::SelectionRange {
                    range: to_proto::range(&line_index, r),
                    parent: Some(Box::new(range)),
                }
            }
            Ok(range)
        })
        .collect();

    Ok(Some(res?))
}

pub(crate) fn handle_matching_brace(
    snap: GlobalStateSnapshot,
    params: lsp_ext::MatchingBraceParams,
) -> anyhow::Result<Vec<Position>> {
    let _p = tracing::info_span!("handle_matching_brace").entered();
    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let line_index = snap.file_line_index(file_id)?;
    params
        .positions
        .iter()
        .map(|position| {
            let offset = from_proto::offset(&line_index, *position);
            offset.map(|offset| {
                let offset = match snap.analysis.matching_brace(FilePosition { file_id, offset }) {
                    Ok(Some(matching_brace_offset)) => matching_brace_offset,
                    Err(_) | Ok(None) => offset,
                };
                to_proto::position(&line_index, offset)
            })
        })
        .collect()
}

pub(crate) fn handle_join_lines(
    snap: GlobalStateSnapshot,
    params: lsp_ext::JoinLinesParams,
) -> anyhow::Result<Vec<lsp_types::TextEdit>> {
    let _p = tracing::info_span!("handle_join_lines").entered();

    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let config = snap.config.join_lines();
    let line_index = snap.file_line_index(file_id)?;

    let mut res = TextEdit::default();
    for range in params.ranges {
        let range = from_proto::text_range(&line_index, range)?;
        let edit = snap.analysis.join_lines(&config, FileRange { file_id, range })?;
        match res.union(edit) {
            Ok(()) => (),
            Err(_edit) => {
                // just ignore overlapping edits
            }
        }
    }

    Ok(to_proto::text_edit_vec(&line_index, res))
}

pub(crate) fn handle_on_enter(
    snap: GlobalStateSnapshot,
    params: lsp_types::TextDocumentPositionParams,
) -> anyhow::Result<Option<Vec<lsp_ext::SnippetTextEdit>>> {
    let _p = tracing::info_span!("handle_on_enter").entered();
    let position = try_default!(from_proto::file_position(&snap, &params)?);
    let edit = match snap.analysis.on_enter(position)? {
        None => return Ok(None),
        Some(it) => it,
    };
    let line_index = snap.file_line_index(position.file_id)?;
    let edit = to_proto::snippet_text_edit_vec(
        &line_index,
        true,
        edit,
        snap.config.change_annotation_support(),
    );
    Ok(Some(edit))
}

pub(crate) fn handle_on_type_formatting(
    snap: GlobalStateSnapshot,
    params: lsp_types::DocumentOnTypeFormattingParams,
) -> anyhow::Result<Option<Vec<lsp_ext::SnippetTextEdit>>> {
    let _p = tracing::info_span!("handle_on_type_formatting").entered();
    let char_typed = params.ch.chars().next().unwrap_or('\0');
    if !snap.config.typing_trigger_chars().contains(char_typed) {
        return Ok(None);
    }
    let tdpp = lsp_types::TextDocumentPositionParams {
        text_document: params.text_document,
        position: params.position,
    };
    let mut position = try_default!(from_proto::file_position(&snap, &tdpp)?);
    let line_index = snap.file_line_index(position.file_id)?;

    // in `ide`, the `on_type` invariant is that
    // `text.char_at(position) == typed_char`.
    position.offset -= TextSize::of('.');

    let text = snap.analysis.file_text(position.file_id)?;
    if stdx::never!(!text[usize::from(position.offset)..].starts_with(char_typed)) {
        return Ok(None);
    }

    let edit = snap.analysis.on_char_typed(position, char_typed)?;
    let edit = match edit {
        Some(it) => it,
        None => return Ok(None),
    };

    // This should be a single-file edit
    let (_, (text_edit, snippet_edit)) = edit.source_file_edits.into_iter().next().unwrap();
    stdx::always!(snippet_edit.is_none(), "on type formatting shouldn't use structured snippets");

    let change = to_proto::snippet_text_edit_vec(
        &line_index,
        edit.is_snippet,
        text_edit,
        snap.config.change_annotation_support(),
    );
    Ok(Some(change))
}

pub(crate) fn empty_diagnostic_report() -> lsp_types::DocumentDiagnosticReport {
    lsp_types::DocumentDiagnosticReport::RelatedFullDocumentDiagnosticReport(
        lsp_types::RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: lsp_types::FullDocumentDiagnosticReport {
                result_id: Some("rust-analyzer".to_owned()),
                items: vec![],
            },
        },
    )
}

pub(crate) fn handle_document_diagnostics(
    snap: GlobalStateSnapshot,
    params: lsp_types::DocumentDiagnosticParams,
) -> anyhow::Result<lsp_types::DocumentDiagnosticReport> {
    let file_id = match from_proto::file_id(&snap, &params.text_document.uri)? {
        Some(it) => it,
        None => return Ok(empty_diagnostic_report()),
    };
    let source_root = snap.analysis.source_root_id(file_id)?;
    if !snap.analysis.is_local_source_root(source_root)? {
        return Ok(empty_diagnostic_report());
    }
    let source_root = snap.analysis.source_root_id(file_id)?;
    let config = snap.config.diagnostics(Some(source_root));
    if !config.enabled {
        return Ok(empty_diagnostic_report());
    }
    let line_index = snap.file_line_index(file_id)?;
    let supports_related = snap.config.text_document_diagnostic_related_document_support();

    let mut related_documents = FxHashMap::default();
    let diagnostics = snap
        .analysis
        .full_diagnostics(&config, AssistResolveStrategy::None, file_id)?
        .into_iter()
        .filter_map(|d| {
            let file = d.range.file_id;
            if file == file_id {
                let diagnostic = convert_diagnostic(&line_index, d);
                return Some(diagnostic);
            }
            if supports_related {
                let (diagnostics, line_index) = related_documents
                    .entry(file)
                    .or_insert_with(|| (Vec::new(), snap.file_line_index(file).ok()));
                let diagnostic = convert_diagnostic(line_index.as_mut()?, d);
                diagnostics.push(diagnostic);
            }
            None
        });
    Ok(lsp_types::DocumentDiagnosticReport::RelatedFullDocumentDiagnosticReport(
        lsp_types::RelatedFullDocumentDiagnosticReport {
            full_document_diagnostic_report: lsp_types::FullDocumentDiagnosticReport {
                result_id: Some("rust-analyzer".to_owned()),
                items: diagnostics.collect(),
            },
            related_documents: related_documents.is_empty().not().then(|| {
                related_documents
                    .into_iter()
                    .map(|(id, (items, _))| {
                        (
                            to_proto::url(&snap, id),
                            lsp_types::RelatedDocument::FullDocumentDiagnosticReport(
                                lsp_types::FullDocumentDiagnosticReport {
                                    result_id: Some("rust-analyzer".to_owned()),
                                    items,
                                },
                            ),
                        )
                    })
                    .collect()
            }),
        },
    ))
}

pub(crate) fn handle_document_symbol(
    snap: GlobalStateSnapshot,
    params: lsp_types::DocumentSymbolParams,
) -> anyhow::Result<Option<lsp_types::DocumentSymbolResponse>> {
    let _p = tracing::info_span!("handle_document_symbol").entered();
    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let line_index = snap.file_line_index(file_id)?;

    let mut symbols: Vec<(lsp_types::DocumentSymbol, Option<usize>)> = Vec::new();

    let config = snap.config.document_symbol(None);

    let structure_nodes = snap.analysis.file_structure(
        &FileStructureConfig { exclude_locals: config.search_exclude_locals },
        file_id,
    )?;

    for node in structure_nodes {
        let mut tags = Vec::new();
        if node.deprecated {
            tags.push(SymbolTag::Deprecated)
        };

        #[allow(deprecated)]
        let symbol = lsp_types::DocumentSymbol {
            name: node.label,
            detail: node.detail,
            kind: to_proto::structure_node_kind(node.kind),
            tags: Some(tags),
            deprecated: Some(node.deprecated),
            range: to_proto::range(&line_index, node.node_range),
            selection_range: to_proto::range(&line_index, node.navigation_range),
            children: None,
        };
        symbols.push((symbol, node.parent));
    }

    // Builds hierarchy from a flat list, in reverse order (so that the indices make sense)
    let document_symbols = {
        let mut acc = Vec::new();
        while let Some((mut symbol, parent_idx)) = symbols.pop() {
            if let Some(children) = &mut symbol.children {
                children.reverse();
            }
            let parent = match parent_idx {
                None => &mut acc,
                Some(i) => symbols[i].0.children.get_or_insert_with(Vec::new),
            };
            parent.push(symbol);
        }
        acc.reverse();
        acc
    };

    let res = if snap.config.hierarchical_symbols() {
        document_symbols.into()
    } else {
        let url = to_proto::url(&snap, file_id);
        let mut symbol_information = Vec::new();
        for symbol in document_symbols {
            flatten_document_symbol(&symbol, None, &url, &mut symbol_information);
        }
        symbol_information.into()
    };
    return Ok(Some(res));

    fn flatten_document_symbol(
        symbol: &lsp_types::DocumentSymbol,
        container_name: Option<String>,
        url: &Uri,
        res: &mut Vec<SymbolInformation>,
    ) {
        #[allow(deprecated)]
        res.push(SymbolInformation {
            deprecated: symbol.deprecated,
            location: Location::new(url.clone(), symbol.range),
            base_symbol_information: lsp_types::BaseSymbolInformation {
                name: symbol.name.clone(),
                kind: symbol.kind,
                tags: symbol.tags.clone(),
                container_name,
            },
        });

        for child in symbol.children.iter().flatten() {
            flatten_document_symbol(child, Some(symbol.name.clone()), url, res);
        }
    }
}

pub(crate) fn handle_workspace_symbol(
    snap: GlobalStateSnapshot,
    params: WorkspaceSymbolParams,
) -> anyhow::Result<Option<lsp_types::WorkspaceSymbolResponse>> {
    let _p = tracing::info_span!("handle_workspace_symbol").entered();

    let config = snap.config.workspace_symbol(None);
    let (all_symbols, libs) = decide_search_kind_and_scope(&params, &config);

    let query = {
        let query: String = params.query.chars().filter(|&c| c != '#' && c != '*').collect();
        let mut q = Query::new(query);
        if !all_symbols {
            q.only_types();
        }
        if libs {
            q.libs();
        }
        if config.search_exclude_imports {
            q.exclude_imports();
        }
        q
    };
    let mut res = exec_query(&snap, query, config.search_limit)?;
    if res.is_empty() && !all_symbols {
        res = exec_query(&snap, Query::new(params.query), config.search_limit)?;
    }

    return Ok(Some(lsp_types::WorkspaceSymbolResponse::WorkspaceSymbolList(res)));

    fn decide_search_kind_and_scope(
        params: &WorkspaceSymbolParams,
        config: &WorkspaceSymbolConfig,
    ) -> (bool, bool) {
        // Support old-style parsing of markers in the query.
        let mut all_symbols = params.query.contains('#');
        let mut libs = params.query.contains('*');

        // If no explicit marker was set, check request params. If that's also empty
        // use global config.
        if !all_symbols {
            let search_kind = match params.search_kind {
                Some(ref search_kind) => search_kind,
                None => &config.search_kind,
            };
            all_symbols = match search_kind {
                lsp_ext::WorkspaceSymbolSearchKind::OnlyTypes => false,
                lsp_ext::WorkspaceSymbolSearchKind::AllSymbols => true,
            }
        }

        if !libs {
            let search_scope = match params.search_scope {
                Some(ref search_scope) => search_scope,
                None => &config.search_scope,
            };
            libs = match search_scope {
                lsp_ext::WorkspaceSymbolSearchScope::Workspace => false,
                lsp_ext::WorkspaceSymbolSearchScope::WorkspaceAndDependencies => true,
            }
        }

        (all_symbols, libs)
    }

    fn exec_query(
        snap: &GlobalStateSnapshot,
        query: Query,
        limit: usize,
    ) -> anyhow::Result<Vec<lsp_types::WorkspaceSymbol>> {
        let mut res = Vec::new();
        for nav in snap.analysis.symbol_search(query, limit)? {
            let container_name = nav.container_name.as_ref().map(|v| v.to_string());

            let info = lsp_types::WorkspaceSymbol {
                location: lsp_types::WorkspaceSymbolLocation::Location(
                    to_proto::location_from_nav(snap, &nav)?,
                ),
                data: None,
                base_symbol_information: lsp_types::BaseSymbolInformation {
                    name: match &nav.alias {
                        Some(alias) => format!("{} (alias for {})", alias, nav.name),
                        None => nav.name.to_string(),
                    },
                    kind: nav
                        .kind
                        .map(to_proto::symbol_kind)
                        .unwrap_or(lsp_types::SymbolKind::Variable),
                    // FIXME: Set deprecation
                    tags: None,
                    container_name,
                },
            };
            res.push(info);
        }
        Ok(res)
    }
}

pub(crate) fn handle_will_rename_files(
    snap: GlobalStateSnapshot,
    params: lsp_types::RenameFilesParams,
) -> anyhow::Result<Option<lsp_types::WorkspaceEdit>> {
    let _p = tracing::info_span!("handle_will_rename_files").entered();

    let source_changes: Vec<SourceChange> = params
        .files
        .into_iter()
        .filter_map(|lsp_types::FileRename { new_uri: to, old_uri: from }| {
            let from_path = from.to_file_path().ok()?;
            let to_path = to.to_file_path().ok()?;

            // Limit to single-level moves for now.
            match (from_path.parent(), to_path.parent()) {
                (Some(p1), Some(p2)) if p1 == p2 => {
                    if from_path.is_dir() {
                        // add '/' to end of url -- from `file://path/to/folder` to `file://path/to/folder/`
                        let mut old_folder_name = from_path.file_stem()?.to_str()?.to_owned();
                        old_folder_name.push('/');
                        let from_with_trailing_slash = from.join(&old_folder_name).ok()?;

                        let imitate_from_url = from_with_trailing_slash.join("mod.rs").ok()?;
                        let new_file_name = to_path.file_name()?.to_str()?;
                        Some((
                            snap.url_to_file_id(&imitate_from_url).ok()?,
                            new_file_name.to_owned(),
                        ))
                    } else {
                        let old_name = from_path.file_stem()?.to_str()?;
                        let new_name = to_path.file_stem()?.to_str()?;
                        match (old_name, new_name) {
                            ("mod", _) => None,
                            (_, "mod") => None,
                            _ => Some((snap.url_to_file_id(&from).ok()?, new_name.to_owned())),
                        }
                    }
                }
                _ => None,
            }
        })
        .filter_map(|(file_id, new_name)| {
            let file_id = file_id?;
            let source_root = snap.analysis.source_root_id(file_id).ok();
            snap.analysis
                .will_rename_file(file_id, &new_name, &snap.config.rename(source_root))
                .ok()?
        })
        .collect::<Vec<_>>();

    // Drop file system edits since we're just renaming things on the same level
    let mut source_changes = source_changes.into_iter();
    let mut source_change = source_changes.next().unwrap_or_default();
    source_change.file_system_edits.clear();
    // no collect here because we want to merge text edits on same file ids
    source_change.extend(source_changes.flat_map(|it| it.source_file_edits));
    if source_change.source_file_edits.is_empty() {
        Ok(None)
    } else {
        Ok(Some(to_proto::workspace_edit(&snap, source_change)?))
    }
}

pub(crate) fn handle_goto_definition(
    snap: GlobalStateSnapshot,
    params: lsp_types::DefinitionParams,
) -> anyhow::Result<Option<lsp_types::DefinitionResponse>> {
    let _p = tracing::info_span!("handle_goto_definition").entered();
    let position =
        try_default!(from_proto::file_position(&snap, &params.text_document_position_params)?);
    let config = snap.config.goto_definition(snap.minicore());
    let nav_info = match snap.analysis.goto_definition(position, &config)? {
        None => return Ok(None),
        Some(it) => it,
    };
    let src = FileRange { file_id: position.file_id, range: nav_info.range };
    let res = to_proto::goto_definition_response(&snap, Some(src), nav_info.info)?;
    Ok(Some(res))
}

pub(crate) fn handle_goto_declaration(
    snap: GlobalStateSnapshot,
    params: lsp_types::DeclarationParams,
) -> anyhow::Result<Option<lsp_types::DeclarationResponse>> {
    let _p = tracing::info_span!("handle_goto_declaration").entered();
    let position =
        try_default!(from_proto::file_position(&snap, &params.text_document_position_params)?);
    let config = snap.config.goto_definition(snap.minicore());
    let nav_info = match snap.analysis.goto_declaration(position, &config)? {
        None => {
            // fallback to goto definition
            let params = lsp_types::DefinitionParams {
                work_done_progress_params: params.work_done_progress_params,
                partial_result_params: params.partial_result_params,
                text_document_position_params: params.text_document_position_params,
            };
            return match handle_goto_definition(snap, params) {
                Ok(Some(x)) => match x {
                    lsp_types::DefinitionResponse::Definition(definition) => {
                        Ok(Some(lsp_types::DeclarationResponse::Declaration(match definition {
                            lsp_types::Definition::Location(location) => {
                                lsp_types::Declaration::Location(location)
                            }
                            lsp_types::Definition::LocationList(locations) => {
                                lsp_types::Declaration::LocationList(locations)
                            }
                        })))
                    }
                    lsp_types::DefinitionResponse::DefinitionLinkList(location_links) => Ok(Some(
                        lsp_types::DeclarationResponse::DeclarationLinkList(location_links),
                    )),
                },
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
        }
        Some(it) => it,
    };
    let src = FileRange { file_id: position.file_id, range: nav_info.range };
    let res = to_proto::goto_declaration_response(&snap, Some(src), nav_info.info)?;
    Ok(Some(res))
}

pub(crate) fn handle_goto_implementation(
    snap: GlobalStateSnapshot,
    params: lsp_types::ImplementationParams,
) -> anyhow::Result<Option<lsp_types::ImplementationResponse>> {
    let _p = tracing::info_span!("handle_goto_implementation").entered();
    let position =
        try_default!(from_proto::file_position(&snap, &params.text_document_position_params)?);
    let nav_info =
        match snap.analysis.goto_implementation(&snap.config.goto_implementation(), position)? {
            None => return Ok(None),
            Some(it) => it,
        };
    let src = FileRange { file_id: position.file_id, range: nav_info.range };
    let res = to_proto::goto_implementation_response(&snap, Some(src), nav_info.info)?;
    Ok(Some(res))
}

pub(crate) fn handle_goto_type_definition(
    snap: GlobalStateSnapshot,
    params: lsp_types::TypeDefinitionParams,
) -> anyhow::Result<Option<lsp_types::TypeDefinitionResponse>> {
    let _p = tracing::info_span!("handle_goto_type_definition").entered();
    let position =
        try_default!(from_proto::file_position(&snap, &params.text_document_position_params)?);
    let nav_info = match snap.analysis.goto_type_definition(position)? {
        None => return Ok(None),
        Some(it) => it,
    };
    let src = FileRange { file_id: position.file_id, range: nav_info.range };
    let res = to_proto::goto_type_definition_response(&snap, Some(src), nav_info.info)?;
    Ok(Some(res))
}

pub(crate) fn handle_parent_module(
    snap: GlobalStateSnapshot,
    params: lsp_types::TextDocumentPositionParams,
) -> anyhow::Result<Option<lsp_types::DefinitionResponse>> {
    let _p = tracing::info_span!("handle_parent_module").entered();
    if let Ok(file_path) = &params.text_document.uri.to_file_path() {
        if file_path.file_name().unwrap_or_default() == "Cargo.toml" {
            // search workspaces for parent packages or fallback to workspace root
            let abs_path_buf = match Utf8PathBuf::from_path_buf(file_path.to_path_buf())
                .ok()
                .map(AbsPathBuf::try_from)
            {
                Some(Ok(abs_path_buf)) => abs_path_buf,
                _ => return Ok(None),
            };

            let manifest_path = match ManifestPath::try_from(abs_path_buf).ok() {
                Some(manifest_path) => manifest_path,
                None => return Ok(None),
            };

            let links: Vec<LocationLink> = snap
                .workspaces
                .iter()
                .filter_map(|ws| match &ws.kind {
                    ProjectWorkspaceKind::Cargo { cargo, .. }
                    | ProjectWorkspaceKind::DetachedFile { cargo: Some((cargo, _, _)), .. } => {
                        cargo.parent_manifests(&manifest_path)
                    }
                    _ => None,
                })
                .flatten()
                .map(|parent_manifest_path| LocationLink {
                    origin_selection_range: None,
                    target_uri: to_proto::url_from_abs_path(&parent_manifest_path),
                    target_range: Range::default(),
                    target_selection_range: Range::default(),
                })
                .collect::<_>();
            return Ok(Some(links.into()));
        }

        // check if invoked at the crate root
        let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
        let crate_id = match snap.analysis.crates_for(file_id)?.first() {
            Some(&crate_id) => crate_id,
            None => return Ok(None),
        };
        let cargo_spec = match TargetSpec::for_file(&snap, file_id)? {
            Some(TargetSpec::Cargo(it)) => it,
            Some(TargetSpec::ProjectJson(_)) | None => return Ok(None),
        };

        if snap.analysis.crate_root(crate_id)? == file_id {
            let cargo_toml_url = to_proto::url_from_abs_path(&cargo_spec.cargo_toml);
            let res = vec![LocationLink {
                origin_selection_range: None,
                target_uri: cargo_toml_url,
                target_range: Range::default(),
                target_selection_range: Range::default(),
            }]
            .into();
            return Ok(Some(res));
        }
    }

    // locate parent module by semantics
    let position = try_default!(from_proto::file_position(&snap, &params)?);
    let navs = snap.analysis.parent_module(position)?;
    let res = to_proto::goto_definition_response(&snap, None, navs)?;
    Ok(Some(res))
}

pub(crate) fn handle_child_modules(
    snap: GlobalStateSnapshot,
    params: lsp_types::TextDocumentPositionParams,
) -> anyhow::Result<Option<lsp_types::DefinitionResponse>> {
    let _p = tracing::info_span!("handle_child_modules").entered();
    // locate child module by semantics
    let position = try_default!(from_proto::file_position(&snap, &params)?);
    let navs = snap.analysis.child_modules(position)?;
    let res = to_proto::goto_definition_response(&snap, None, navs)?;
    Ok(Some(res))
}

pub(crate) fn handle_runnables(
    snap: GlobalStateSnapshot,
    params: lsp_ext::RunnablesParams,
) -> anyhow::Result<Vec<lsp_ext::Runnable>> {
    let _p = tracing::info_span!("handle_runnables").entered();
    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let source_root = snap.analysis.source_root_id(file_id).ok();
    let line_index = snap.file_line_index(file_id)?;
    let offset = params.position.and_then(|it| from_proto::offset(&line_index, it).ok());
    let target_spec = TargetSpec::for_file(&snap, file_id)?;

    let mut res = Vec::new();
    for runnable in snap.analysis.runnables(file_id)? {
        if should_skip_for_offset(&runnable, offset)
            || should_skip_target(&runnable, target_spec.as_ref())
        {
            continue;
        }

        let update_test = runnable.update_test;
        if let Some(mut runnable) = to_proto::runnable(&snap, runnable)? {
            if let Some(runnable) = to_proto::make_update_runnable(&runnable, update_test) {
                res.push(runnable);
            }

            if let lsp_ext::RunnableArgs::Cargo(r) = &mut runnable.args
                && let Some(TargetSpec::Cargo(CargoTargetSpec {
                    sysroot_root: Some(sysroot_root),
                    ..
                })) = &target_spec
            {
                r.environment.insert("RUSTC_TOOLCHAIN".to_owned(), sysroot_root.to_string());
            };

            res.push(runnable);
        }
    }

    // Add `cargo check` and `cargo test` for all targets of the whole package
    let config = snap.config.runnables(source_root);
    match target_spec {
        Some(TargetSpec::Cargo(spec)) => {
            let is_crate_no_std = snap.analysis.is_crate_no_std(spec.crate_id)?;
            for cmd in ["check", "run", "test"] {
                if cmd == "run" && spec.target_kind != TargetKind::Bin {
                    continue;
                }
                let cwd = if cmd != "test" || spec.target_kind == TargetKind::Bin {
                    spec.workspace_root.clone()
                } else {
                    spec.cargo_toml.parent().to_path_buf()
                };
                let mut cargo_args =
                    vec![cmd.to_owned(), "--package".to_owned(), spec.package.clone()];
                let all_targets = cmd != "run" && !is_crate_no_std;
                if all_targets {
                    cargo_args.push("--all-targets".to_owned());
                }
                cargo_args.extend(config.cargo_extra_args.iter().cloned());
                if let Some(config_path) = &config.config_path {
                    cargo_args.push("--config".to_owned());
                    cargo_args.push(config_path.to_string());
                }
                res.push(lsp_ext::Runnable {
                    label: format!(
                        "cargo {cmd} -p {}{all_targets}",
                        spec.package,
                        all_targets = if all_targets { " --all-targets" } else { "" }
                    ),
                    location: None,
                    args: lsp_ext::RunnableArgs::Cargo(lsp_ext::CargoRunnableArgs {
                        workspace_root: Some(spec.workspace_root.clone().into()),
                        cwd: cwd.into(),
                        override_cargo: config.override_cargo.clone(),
                        cargo_args,
                        executable_args: Vec::new(),
                        environment: spec
                            .sysroot_root
                            .as_ref()
                            .map(|root| ("RUSTC_TOOLCHAIN".to_owned(), root.to_string()))
                            .into_iter()
                            .collect(),
                    }),
                })
            }
        }
        Some(TargetSpec::ProjectJson(_)) => {}
        None => {
            if !snap.config.linked_or_discovered_projects().is_empty()
                && let Some(path) = snap.file_id_to_file_path(file_id).parent()
            {
                let mut cargo_args = vec!["check".to_owned(), "--workspace".to_owned()];
                cargo_args.extend(config.cargo_extra_args.iter().cloned());
                if let Some(config_path) = &config.config_path {
                    cargo_args.push("--config".to_owned());
                    cargo_args.push(config_path.to_string());
                }
                res.push(lsp_ext::Runnable {
                    label: "cargo check --workspace".to_owned(),
                    location: None,
                    args: lsp_ext::RunnableArgs::Cargo(lsp_ext::CargoRunnableArgs {
                        workspace_root: None,
                        cwd: path.as_path().unwrap().to_path_buf().into(),
                        override_cargo: config.override_cargo,
                        cargo_args,
                        executable_args: Vec::new(),
                        environment: Default::default(),
                    }),
                });
            };
        }
    }
    Ok(res)
}

fn should_skip_for_offset(runnable: &Runnable, offset: Option<TextSize>) -> bool {
    match offset {
        None => false,
        _ if matches!(&runnable.kind, RunnableKind::TestMod { .. }) => false,
        Some(offset) => !runnable.nav.full_range.contains_inclusive(offset),
    }
}

pub(crate) fn handle_related_tests(
    snap: GlobalStateSnapshot,
    params: lsp_types::TextDocumentPositionParams,
) -> anyhow::Result<Vec<lsp_ext::TestInfo>> {
    let _p = tracing::info_span!("handle_related_tests").entered();
    let position = try_default!(from_proto::file_position(&snap, &params)?);

    let tests = snap.analysis.related_tests(position, None)?;
    let mut res = Vec::new();
    for it in tests {
        if let Ok(Some(runnable)) = to_proto::runnable(&snap, it) {
            res.push(lsp_ext::TestInfo { runnable })
        }
    }

    Ok(res)
}

pub(crate) fn handle_completion(
    snap: GlobalStateSnapshot,
    lsp_types::CompletionParams {
        text_document_position_params,
        context,
        ..
    }: lsp_types::CompletionParams,
) -> anyhow::Result<Option<lsp_types::CompletionResponse>> {
    let _p = tracing::info_span!("handle_completion").entered();
    let mut position =
        try_default!(from_proto::file_position(&snap, &text_document_position_params)?);
    let line_index = snap.file_line_index(position.file_id)?;
    let completion_trigger_character =
        context.and_then(|ctx| ctx.trigger_character).and_then(|s| s.chars().next());

    let source_root = snap.analysis.source_root_id(position.file_id)?;
    let completion_config = &snap.config.completion(Some(source_root), snap.minicore());
    // FIXME: We should fix up the position when retrying the cancelled request instead
    position.offset = position.offset.min(line_index.index.len());
    let items = match snap.analysis.completions(
        completion_config,
        position,
        completion_trigger_character,
    )? {
        None => return Ok(None),
        Some(items) => items,
    };

    let items = to_proto::completion_items(
        &snap.config,
        &completion_config.fields_to_resolve,
        &line_index,
        snap.file_version(position.file_id),
        &text_document_position_params,
        completion_trigger_character,
        items,
    );

    let completion_list = lsp_types::CompletionList {
        is_incomplete: true,
        items,
        item_defaults: None,
        apply_kind: None,
    };
    Ok(Some(completion_list.into()))
}

pub(crate) fn handle_completion_resolve(
    snap: GlobalStateSnapshot,
    mut original_completion: CompletionItem,
) -> anyhow::Result<CompletionItem> {
    let _p = tracing::info_span!("handle_completion_resolve").entered();

    if !all_edits_are_disjoint(&original_completion, &[]) {
        return Err(invalid_params_error(
            "Received a completion with overlapping edits, this is not LSP-compliant".to_owned(),
        )
        .into());
    }

    let Some(data) = original_completion.data.take() else {
        return Ok(original_completion);
    };

    let resolve_data: lsp_ext::CompletionResolveData = serde_json::from_value(data)?;

    let file_id = from_proto::file_id(&snap, &resolve_data.position.text_document.uri)?
        .expect("we never provide completions for excluded files");
    let line_index = snap.file_line_index(file_id)?;
    // FIXME: We should fix up the position when retrying the cancelled request instead
    let Ok(offset) = from_proto::offset(&line_index, resolve_data.position.position) else {
        return Ok(original_completion);
    };
    let source_root = snap.analysis.source_root_id(file_id)?;

    let mut forced_resolve_completions_config =
        snap.config.completion(Some(source_root), snap.minicore());
    forced_resolve_completions_config.fields_to_resolve = CompletionFieldsToResolve::empty();

    let position = FilePosition { file_id, offset };
    let Some(completions) = snap.analysis.completions(
        &forced_resolve_completions_config,
        position,
        resolve_data.trigger_character,
    )?
    else {
        return Ok(original_completion);
    };
    let Ok(resolve_data_hash) = BASE64_STANDARD.decode(resolve_data.hash) else {
        return Ok(original_completion);
    };

    let Some(corresponding_completion) = completions.into_iter().find(|completion_item| {
        // Avoid computing hashes for items that obviously do not match
        // r-a might append a detail-based suffix to the label, so we cannot check for equality
        original_completion.label.starts_with(completion_item.label.primary.as_str())
            && resolve_data_hash == completion_item_hash(completion_item, resolve_data.for_ref)
    }) else {
        return Ok(original_completion);
    };

    let mut resolved_completions = to_proto::completion_items(
        &snap.config,
        &forced_resolve_completions_config.fields_to_resolve,
        &line_index,
        snap.file_version(position.file_id),
        &resolve_data.position,
        resolve_data.trigger_character,
        vec![corresponding_completion],
    );
    let Some(mut resolved_completion) = resolved_completions.pop() else {
        return Ok(original_completion);
    };

    if !resolve_data.imports.is_empty() {
        let additional_edits = snap
            .analysis
            .resolve_completion_edits(
                &forced_resolve_completions_config,
                position,
                resolve_data.imports.into_iter().map(|import| CompletionItemImport {
                    path: import.full_import_path,
                    as_underscore: import.as_underscore,
                }),
            )?
            .into_iter()
            .flat_map(|edit| edit.into_iter().map(|indel| to_proto::text_edit(&line_index, indel)))
            .collect::<Vec<_>>();

        if !all_edits_are_disjoint(&resolved_completion, &additional_edits) {
            return Err(LspError::new(
                ErrorCode::InternalError as i32,
                "Import edit overlaps with the original completion edits, this is not LSP-compliant"
                    .into(),
            )
            .into());
        }

        if let Some(original_additional_edits) = resolved_completion.additional_text_edits.as_mut()
        {
            original_additional_edits.extend(additional_edits)
        } else {
            resolved_completion.additional_text_edits = Some(additional_edits);
        }
    }

    Ok(resolved_completion)
}

pub(crate) fn handle_folding_range(
    snap: GlobalStateSnapshot,
    params: FoldingRangeParams,
) -> anyhow::Result<Option<Vec<FoldingRange>>> {
    let _p = tracing::info_span!("handle_folding_range").entered();

    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let collapsed_text = snap.config.folding_range_collapsed_text();
    let folds = snap.analysis.folding_ranges(file_id, collapsed_text)?;

    let text = snap.analysis.file_text(file_id)?;
    let line_index = snap.file_line_index(file_id)?;
    let line_folding_only = snap.config.line_folding_only();

    let res = folds
        .into_iter()
        .map(|it| to_proto::folding_range(&text, &line_index, line_folding_only, it))
        .collect();
    Ok(Some(res))
}

pub(crate) fn handle_signature_help(
    snap: GlobalStateSnapshot,
    params: lsp_types::SignatureHelpParams,
) -> anyhow::Result<Option<lsp_types::SignatureHelp>> {
    let _p = tracing::info_span!("handle_signature_help").entered();
    let position =
        try_default!(from_proto::file_position(&snap, &params.text_document_position_params)?);
    let help = match snap.analysis.signature_help(position)? {
        Some(it) => it,
        None => return Ok(None),
    };
    let config = snap.config.call_info();
    let res = to_proto::signature_help(help, config, snap.config.signature_help_label_offsets());
    Ok(Some(res))
}

pub(crate) fn handle_hover(
    snap: GlobalStateSnapshot,
    params: lsp_ext::HoverParams,
) -> anyhow::Result<Option<lsp_ext::Hover>> {
    let _p = tracing::info_span!("handle_hover").entered();
    let request_range = match params.position {
        PositionOrRange::Position(position) => Range::new(position, position),
        PositionOrRange::Range(range) => range,
    };
    let file_range =
        try_default!(from_proto::file_range(&snap, &params.text_document, request_range)?);
    let events = ownership_events_for_file(&snap.ownership_events, file_range.file_id);
    let selected = events.iter().find(|event| {
        event.range.start <= request_range.start && request_range.start <= event.range.end
            || event.binding_range.start <= request_range.start
                && request_range.start <= event.binding_range.end
    });
    let line_index = snap.file_line_index(file_range.file_id)?;
    let alternatives = ownership_alternatives(
        &snap,
        &params.text_document.uri,
        file_range.file_id,
        &line_index,
        request_range,
        selected,
    )?;

    let hover_config = snap.config.hover(snap.minicore());
    let info = match snap.analysis.hover(&hover_config, file_range)? {
        None => {
            let mut sections = Vec::new();
            if let Some(alternatives) = alternatives {
                sections.push(alternatives);
            }
            if let Some(selected) = selected {
                sections.push(ownership_timeline(&events, selected));
            }
            if sections.is_empty() {
                return Ok(None);
            }
            return Ok(Some(lsp_ext::Hover {
                hover: lsp_types::Hover {
                    contents: Contents::MarkupContent(lsp_types::MarkupContent {
                        kind: lsp_types::MarkupKind::Markdown,
                        value: sections.join("\n\n---\n\n"),
                    }),
                    range: Some(selected.map_or(request_range, |event| event.range)),
                },
                actions: Vec::new(),
            }));
        }
        Some(info) => info,
    };

    let range = to_proto::range(&line_index, info.range);
    let markup_kind = hover_config.format;
    let mut hover = lsp_ext::Hover {
        hover: lsp_types::Hover {
            contents: Contents::MarkupContent(to_proto::markup_content(
                info.info.markup,
                markup_kind,
            )),
            range: Some(range),
        },
        actions: if snap.config.hover_actions().none() {
            Vec::new()
        } else {
            prepare_hover_actions(&snap, &info.info.actions)
        },
    };

    let mut ownership_sections = Vec::new();
    if let Some(alternatives) = alternatives {
        ownership_sections.push(alternatives);
    }
    if let Some(selected) = selected {
        ownership_sections.push(ownership_timeline(&events, selected));
    }
    if !ownership_sections.is_empty() {
        let Contents::MarkupContent(markup) = &mut hover.hover.contents else {
            return Ok(Some(hover));
        };
        format_to!(markup.value, "\n\n---\n\n{}", ownership_sections.join("\n\n---\n\n"),);
    }

    Ok(Some(hover))
}

pub(crate) fn handle_ownership_model(
    snap: GlobalStateSnapshot,
    params: lsp_types::TextDocumentPositionParams,
) -> anyhow::Result<lsp_ext::OwnershipModelResult> {
    let _p = tracing::info_span!("handle_ownership_model").entered();
    let request_range = Range::new(params.position, params.position);
    let file_range =
        try_default!(from_proto::file_range(&snap, &params.text_document, request_range,)?);
    let all_events = ownership_events_for_file(&snap.ownership_events, file_range.file_id);
    let tutorial = crate::diagnostics::ownership_tutorial_for_file(
        &snap.ownership_tutorial_models,
        file_range.file_id,
    );
    let diagnostics =
        ownership_diagnostics_for_file(&snap.ownership_diagnostics, file_range.file_id);
    let selected_diagnostic = diagnostics.iter().find(|diagnostic| {
        ranges_touch(diagnostic.range, request_range)
            || diagnostic.related.iter().any(|related| ranges_touch(related.range, request_range))
    });
    let selected = all_events.iter().find(|event| {
        ranges_touch(event.range, request_range) || ranges_touch(event.binding_range, request_range)
    });
    let (mut selected_body_id, mut selected_name) =
        ownership_selection_context(selected, &tutorial, request_range);
    if selected.is_none() {
        selected_body_id = selected_body_id.or_else(|| {
            tutorial
                .bodies
                .iter()
                .find(|body| ranges_touch(body.range, request_range))
                .map(|body| body.body_id)
        });
        selected_name = selected_name.or_else(|| {
            selected_diagnostic
                .and_then(|diagnostic| ownership_name_from_message(&diagnostic.message))
                .map(ownership_root_name)
                .map(str::to_owned)
        });
    }
    const MAX_INTERACTIVE_EVENTS: usize = 256;
    const MAX_INTERACTIVE_BLOCKS: usize = 512;
    const MAX_INTERACTIVE_BINDINGS: usize = 64;
    const MAX_INTERACTIVE_LOANS: usize = 64;
    const MAX_INTERACTIVE_MEMORY_NODES: usize = 128;
    const MAX_INTERACTIVE_MEMORY_EDGES: usize = 256;
    const MAX_INTERACTIVE_SNAPSHOTS: usize = 256;
    const MAX_INTERACTIVE_ACCESS_PATHS: usize = 64;
    let mut events = all_events
        .iter()
        .filter(|event| {
            ownership_event_in_context(event, selected, selected_body_id, selected_name.as_deref())
        })
        .map(|event| lsp_ext::OwnershipModelEvent {
            event_id: event.event_id.clone(),
            body_id: event.body_id,
            basic_block: event.basic_block,
            statement_index: event.statement_index,
            kind: ownership_kind_code(event.kind).to_owned(),
            state: ownership_state_code(event.state).to_owned(),
            place: event.place.clone(),
            loan_id: event.loan_id,
            range: event.range,
            binding_range: event.binding_range,
            detail: event.detail.clone(),
            destination: event.destination.as_ref().map(|destination| {
                lsp_ext::OwnershipModelEventDestination {
                    kind: destination.kind.clone(),
                    label: destination.label.clone(),
                    place: destination.place.clone(),
                    range: destination.range,
                }
            }),
        })
        .collect::<Vec<_>>();
    let mut response_truncated = events.len() > MAX_INTERACTIVE_EVENTS;
    events.truncate(MAX_INTERACTIVE_EVENTS);
    let source = snap.analysis.file_text(file_range.file_id)?;
    let line_index = snap.file_line_index(file_range.file_id)?;
    let syntax = snap.analysis.parse(file_range.file_id).ok();
    let conflict_graph = selected_diagnostic.and_then(|diagnostic| {
        ownership_conflict_graph(
            diagnostic,
            syntax.as_ref()?,
            &line_index,
            &tutorial,
            selected_body_id,
        )
    });
    let language_fixes =
        ownership_language_fixes_for_diagnostic(&snap, file_range.file_id, selected_diagnostic);
    let mut repairs = language_fixes
        .into_iter()
        .enumerate()
        .filter_map(|(index, fix)| {
            let diff = ownership_fix_diff(&source, &line_index, &params.text_document.uri, fix)?;
            let compiler_validated = fix.action.is_preferred.unwrap_or(false);
            Some(lsp_ext::OwnershipModelRepair {
                id: ownership_language_repair_id(index),
                title: fix.action.title.clone(),
                strategy: "language_fix".to_owned(),
                semantics: "ordinary Rust ownership and compile-time checked access".to_owned(),
                diff,
                compiler_validated,
                validation_state: if compiler_validated {
                    "validated".to_owned()
                } else {
                    "candidate".to_owned()
                },
                effects: ownership_language_repair_effects(&fix.action.title),
                preview_graph: None,
            })
        })
        .collect::<Vec<_>>();
    let validated_fixes = ownership_fixes_at(&snap, file_range.file_id, request_range, selected);
    let mut validated_strategies = Vec::new();
    repairs.extend(validated_fixes.into_iter().enumerate().filter_map(|(index, fix)| {
        let strategy = fix.ownership_wrapper?;
        validated_strategies.push(strategy);
        let diff = ownership_fix_diff(&source, &line_index, &params.text_document.uri, fix)?;
        Some(lsp_ext::OwnershipModelRepair {
            id: ownership_repair_id(strategy, index),
            title: fix.action.title.clone(),
            strategy: ownership_wrapper_code(strategy).to_owned(),
            semantics: strategy.runtime_semantics().to_owned(),
            diff,
            compiler_validated: true,
            validation_state: "validated".to_owned(),
            effects: ownership_repair_effects(strategy),
            preview_graph: ownership_repair_preview_graph(
                strategy,
                selected_name.as_deref().unwrap_or("value"),
                request_range,
                true,
            ),
        })
    }));
    // A diagnostic often points at a later, invalid use while the assist must be
    // computed at the original binding. Prefer the live syntax tree here: the
    // compact compiler model may omit a binding which has no exact MIR event at
    // the request position, and macro arguments do not expose an assist target.
    let candidate_name = selected_diagnostic
        .and_then(|diagnostic| ownership_name_from_message(&diagnostic.message))
        .or(selected_name.as_deref())
        .map(ownership_root_name);
    let request_offset: u32 = file_range.range.start().into();
    let syntax_candidate_range = candidate_name.and_then(|candidate_name| {
        syntax
            .as_ref()?
            .syntax()
            .descendants()
            .filter_map(ast::IdentPat::cast)
            .filter_map(|pattern| {
                let name = pattern.name()?;
                (name.text().to_string() == candidate_name).then(|| name.syntax().text_range())
            })
            .min_by_key(|range| {
                let start: u32 = range.start().into();
                start.abs_diff(request_offset)
            })
    });
    let candidate_range = syntax_candidate_range
        .or_else(|| {
            tutorial
                .bindings
                .iter()
                .find(|binding| {
                    selected_body_id.is_none_or(|body_id| binding.body_id == body_id)
                        && selected_name.as_deref().is_some_and(|name| {
                            ownership_name_refers_to_binding(name, &binding.name)
                                || ownership_name_refers_to_binding(&binding.name, name)
                        })
                })
                .and_then(|binding| from_proto::text_range(&line_index, binding.range).ok())
        })
        .or_else(|| {
            selected.and_then(|event| from_proto::text_range(&line_index, event.binding_range).ok())
        })
        .unwrap_or(file_range.range);
    for (index, fix) in
        ownership_candidate_fixes(&snap, file_range.file_id, candidate_range, request_range)?
            .iter()
            .enumerate()
    {
        let Some(strategy) = fix.ownership_wrapper else { continue };
        if validated_strategies.contains(&strategy) {
            continue;
        }
        let Some(diff) = ownership_fix_diff(&source, &line_index, &params.text_document.uri, fix)
        else {
            continue;
        };
        repairs.push(lsp_ext::OwnershipModelRepair {
            id: ownership_repair_id(strategy, index),
            title: fix.action.title.trim_end_matches(" (unvalidated)").to_owned(),
            strategy: ownership_wrapper_code(strategy).to_owned(),
            semantics: strategy.runtime_semantics().to_owned(),
            diff,
            compiler_validated: false,
            validation_state: "candidate".to_owned(),
            effects: ownership_repair_effects(strategy),
            preview_graph: ownership_repair_preview_graph(
                strategy,
                selected_name.as_deref().unwrap_or("value"),
                request_range,
                false,
            ),
        });
    }
    let exact_events = events.iter().any(|event| {
        all_events
            .iter()
            .find(|candidate| candidate.event_id == event.event_id)
            .is_some_and(|event| event.exact)
    });

    let bodies = tutorial
        .bodies
        .iter()
        .filter(|body| selected_body_id.is_none_or(|body_id| body.body_id == body_id))
        .map(|body| {
            response_truncated |= body.blocks.len() > MAX_INTERACTIVE_BLOCKS;
            lsp_ext::OwnershipModelBody {
                body_id: body.body_id,
                name: body.name.clone(),
                range: body.range,
                blocks: body
                    .blocks
                    .iter()
                    .take(MAX_INTERACTIVE_BLOCKS)
                    .map(|block| lsp_ext::OwnershipModelBlock {
                        basic_block: block.basic_block,
                        range: block.range,
                        successors: block.successors.clone(),
                    })
                    .collect(),
                provenance: "compiler_exact".to_owned(),
            }
        })
        .collect::<Vec<_>>();
    let matching_binding_count = tutorial
        .bindings
        .iter()
        .filter(|binding| {
            selected_body_id.is_none_or(|body_id| binding.body_id == body_id)
                && selected_name.as_deref().is_none_or(|name| binding.name == name)
        })
        .count();
    response_truncated |= matching_binding_count > MAX_INTERACTIVE_BINDINGS;
    let bindings = tutorial
        .bindings
        .iter()
        .filter(|binding| {
            selected_body_id.is_none_or(|body_id| binding.body_id == body_id)
                && selected_name.as_deref().is_none_or(|name| binding.name == name)
        })
        .take(MAX_INTERACTIVE_BINDINGS)
        .map(|binding| lsp_ext::OwnershipModelBinding {
            id: format!("{:016x}-{}", binding.body_id, binding.name),
            body_id: binding.body_id,
            name: binding.name.clone(),
            range: binding.range,
            type_name: binding.type_name.clone(),
            size: binding.size,
            align: binding.align,
            memory_layers: binding
                .memory_layers
                .iter()
                .map(|layer| lsp_ext::OwnershipModelMemoryLayer {
                    kind: layer.kind.clone(),
                    storage: layer.storage.clone(),
                    label: layer.label.clone(),
                    type_name: layer.type_name.clone(),
                    size: layer.size,
                    align: layer.align,
                    provenance: layer.provenance.clone(),
                })
                .collect(),
            provenance: "compiler_exact".to_owned(),
        })
        .collect::<Vec<_>>();
    let matching_loan_count = tutorial
        .loans
        .iter()
        .filter(|loan| {
            selected_body_id.is_none_or(|body_id| loan.body_id == body_id)
                && selected_name.as_deref().is_none_or(|name| loan.name == name)
        })
        .count();
    response_truncated |= matching_loan_count > MAX_INTERACTIVE_LOANS;
    response_truncated |= tutorial.loans.iter().any(|loan| {
        selected_body_id.is_none_or(|body_id| loan.body_id == body_id)
            && selected_name.as_deref().is_none_or(|name| loan.name == name)
            && (loan.live_points.len() > 512 || loan.end_points.len() > 64)
    });
    let loans = tutorial
        .loans
        .iter()
        .filter(|loan| {
            selected_body_id.is_none_or(|body_id| loan.body_id == body_id)
                && selected_name.as_deref().is_none_or(|name| loan.name == name)
        })
        .take(MAX_INTERACTIVE_LOANS)
        .map(ownership_loan_to_lsp)
        .collect::<Vec<_>>();
    // Start at the exact selected place, then retain its bounded connected component. Filtering
    // every node by the selected spelling used to discard move destinations and sibling Rc/Arc
    // handles, leaving the editor with a misleading one-node picture.
    let mut relevant_memory_node_ids = tutorial
        .memory_graph
        .nodes
        .iter()
        .filter(|node| selected_body_id.is_none_or(|body_id| node.node.body_id == body_id))
        .filter(|node| {
            selected_name.as_deref().is_none_or(|name| {
                ownership_name_refers_to_binding(name, &node.node.place)
                    || ownership_name_refers_to_binding(&node.node.place, name)
            })
        })
        .map(|node| node.node.id.clone())
        .collect::<FxHashSet<_>>();
    if selected_name.is_none() {
        relevant_memory_node_ids.extend(
            tutorial
                .memory_graph
                .nodes
                .iter()
                .filter(|node| selected_body_id.is_none_or(|body_id| node.node.body_id == body_id))
                .map(|node| node.node.id.clone()),
        );
    } else {
        for _ in 0..12 {
            let before = relevant_memory_node_ids.len();
            for edge in &tutorial.memory_graph.edges {
                if relevant_memory_node_ids.contains(&edge.edge.source) {
                    relevant_memory_node_ids.insert(edge.edge.target.clone());
                }
                if relevant_memory_node_ids.contains(&edge.edge.target) {
                    relevant_memory_node_ids.insert(edge.edge.source.clone());
                }
            }
            if relevant_memory_node_ids.len() == before {
                break;
            }
        }
    }
    let memory_node_matches = |node: &crate::diagnostics::OwnershipTutorialMemoryNode| {
        selected_body_id.is_none_or(|body_id| node.node.body_id == body_id)
            && relevant_memory_node_ids.contains(&node.node.id)
    };
    let matching_memory_node_count =
        tutorial.memory_graph.nodes.iter().filter(|node| memory_node_matches(node)).count();
    response_truncated |= matching_memory_node_count > MAX_INTERACTIVE_MEMORY_NODES;
    let memory_nodes = tutorial
        .memory_graph
        .nodes
        .iter()
        .filter(|node| memory_node_matches(node))
        .take(MAX_INTERACTIVE_MEMORY_NODES)
        .map(|tutorial_node| {
            let node = &tutorial_node.node;
            lsp_ext::OwnershipModelMemoryNode {
                id: node.id.clone(),
                body_id: node.body_id,
                place: node.place.clone(),
                kind: node.kind.clone(),
                storage: node.storage.clone(),
                label: node.label.clone(),
                type_name: node.type_name.clone(),
                size: node.size,
                align: node.align,
                range: tutorial_node.range,
                state: ownership_state_code(node.state).to_owned(),
                provenance: node.provenance.clone(),
                physical_placement_note: node.physical_placement_note.clone(),
                truncated: node.truncated,
            }
        })
        .collect::<Vec<_>>();
    let memory_node_is_present = |id: &str| memory_nodes.iter().any(|node| node.id == id);
    let matching_memory_edge_count = tutorial
        .memory_graph
        .edges
        .iter()
        .filter(|edge| {
            memory_node_is_present(&edge.edge.source) && memory_node_is_present(&edge.edge.target)
        })
        .count();
    response_truncated |= matching_memory_edge_count > MAX_INTERACTIVE_MEMORY_EDGES;
    let memory_edges = tutorial
        .memory_graph
        .edges
        .iter()
        .filter(|edge| {
            memory_node_is_present(&edge.edge.source) && memory_node_is_present(&edge.edge.target)
        })
        .take(MAX_INTERACTIVE_MEMORY_EDGES)
        .map(|tutorial_edge| {
            let edge = &tutorial_edge.edge;
            lsp_ext::OwnershipModelMemoryEdge {
                id: edge.id.clone(),
                source: edge.source.clone(),
                target: edge.target.clone(),
                relation: edge.relation.clone(),
                event_id: edge.event_id.clone(),
                loan_id: edge.loan_id,
                range: tutorial_edge.range.or_else(|| {
                    edge.event_id
                        .as_deref()
                        .and_then(|event_id| events.iter().find(|event| event.event_id == event_id))
                        .map(|event| event.range)
                }),
                provenance: edge.provenance.clone(),
                path_marker: edge.path_marker.clone(),
            }
        })
        .collect::<Vec<_>>();
    let matching_snapshot_count = tutorial
        .memory_graph
        .snapshots
        .iter()
        .filter(|snapshot| {
            selected_body_id.is_none_or(|body_id| snapshot.snapshot.body_id == body_id)
                && selected_name.as_deref().is_none_or(|name| {
                    ownership_name_refers_to_binding(name, &snapshot.snapshot.place)
                        || ownership_name_refers_to_binding(&snapshot.snapshot.place, name)
                })
        })
        .count();
    response_truncated |= matching_snapshot_count > MAX_INTERACTIVE_SNAPSHOTS;
    let memory_snapshots = tutorial
        .memory_graph
        .snapshots
        .iter()
        .filter(|snapshot| {
            selected_body_id.is_none_or(|body_id| snapshot.snapshot.body_id == body_id)
                && selected_name.as_deref().is_none_or(|name| {
                    ownership_name_refers_to_binding(name, &snapshot.snapshot.place)
                        || ownership_name_refers_to_binding(&snapshot.snapshot.place, name)
                })
        })
        .take(MAX_INTERACTIVE_SNAPSHOTS)
        .map(|tutorial_snapshot| {
            let snapshot = &tutorial_snapshot.snapshot;
            lsp_ext::OwnershipModelSnapshot {
                id: snapshot.id.clone(),
                event_id: snapshot.event_id.clone(),
                body_id: snapshot.body_id,
                basic_block: snapshot.basic_block,
                statement_index: snapshot.statement_index,
                kind: snapshot.kind.clone(),
                range: tutorial_snapshot.range,
                place: snapshot.place.clone(),
                loan_id: snapshot.loan_id,
                path_marker: snapshot.path_marker.clone(),
                deltas: snapshot
                    .deltas
                    .iter()
                    .map(|delta| lsp_ext::OwnershipModelStateDelta {
                        node_id: delta.node_id.clone(),
                        from: delta.from.map(|state| ownership_state_code(state).to_owned()),
                        to: ownership_state_code(delta.to).to_owned(),
                        relation_added: delta.relation_added.clone(),
                        relation_removed: delta.relation_removed.clone(),
                    })
                    .collect(),
                provenance: snapshot.provenance.clone(),
            }
        })
        .collect::<Vec<_>>();
    let matching_access_path_count = tutorial
        .memory_graph
        .access_paths
        .iter()
        .filter(|path| memory_node_is_present(&path.node_id))
        .count();
    response_truncated |= matching_access_path_count > MAX_INTERACTIVE_ACCESS_PATHS;
    let access_paths = tutorial
        .memory_graph
        .access_paths
        .iter()
        .filter(|path| memory_node_is_present(&path.node_id))
        .take(MAX_INTERACTIVE_ACCESS_PATHS)
        .map(|path| lsp_ext::OwnershipModelAccessPath {
            id: path.id.clone(),
            body_id: path.body_id,
            node_id: path.node_id.clone(),
            place: path.place.clone(),
            purpose: path.purpose.clone(),
            steps: path
                .steps
                .iter()
                .map(|step| lsp_ext::OwnershipModelAccessStep {
                    kind: step.kind.clone(),
                    starting_type: step.starting_type.clone(),
                    result_type: step.result_type.clone(),
                    mutability: step.mutability.clone(),
                    explicitness: step.explicitness.clone(),
                    fallible: step.fallible,
                    may_panic: step.may_panic,
                    requires_unsafe: step.requires_unsafe,
                    explanation: step.explanation.clone(),
                    provenance: step.provenance.clone(),
                })
                .collect(),
            provenance: path.provenance.clone(),
        })
        .collect::<Vec<_>>();
    response_truncated |= tutorial.memory_graph.truncated;
    let memory_graph = lsp_ext::OwnershipModelMemoryGraph {
        nodes: memory_nodes,
        edges: memory_edges,
        snapshots: memory_snapshots,
        access_paths,
        truncated: tutorial.memory_graph.truncated,
    };
    let has_tutorial_facts = !bodies.is_empty()
        || !bindings.is_empty()
        || !loans.is_empty()
        || !memory_graph.nodes.is_empty();
    let exact = exact_events || (tutorial.schema_version >= 3 && has_tutorial_facts);
    let c_sketch = ownership_c_sketch(selected_name.as_deref(), &bindings, &events, &repairs);
    // Explain every resolved call which appears on the selected ownership flow, not merely the
    // token carrying the final diagnostic. A single move/borrow error often has its cause and its
    // rejected use on different statements. Keep the request bounded and de-duplicate calls whose
    // ranges are referenced by several MIR events.
    let mut operation_positions = vec![file_range.range.start()];
    if let Some(diagnostic) = selected_diagnostic {
        operation_positions.extend(
            [diagnostic.range.start, diagnostic.range.end]
                .into_iter()
                .chain(
                    diagnostic
                        .related
                        .iter()
                        .flat_map(|related| [related.range.start, related.range.end]),
                )
                .filter_map(|position| from_proto::offset(&line_index, position).ok()),
        );
    }
    operation_positions.extend(
        events
            .iter()
            .take(MAX_INTERACTIVE_EVENTS)
            .filter_map(|event| from_proto::offset(&line_index, event.range.start).ok()),
    );
    operation_positions.sort_unstable();
    operation_positions.dedup();
    let operation_positions = operation_positions
        .into_iter()
        .take(24)
        .map(|offset| FilePosition { file_id: file_range.file_id, offset })
        .collect();
    let mut operation_insights =
        snap.analysis.ownership_call_insights_for_positions(operation_positions)?;
    operation_insights.sort_by_key(|operation| operation.range.start());
    operation_insights.dedup_by_key(|operation| operation.range);
    response_truncated |= operation_insights.len() > 64;
    let operations = operation_insights
        .into_iter()
        .take(64)
        .enumerate()
        .map(|(index, operation)| lsp_ext::OwnershipOperationInsight {
            id: format!(
                "operation-{index}-{}-{}",
                u32::from(operation.range.start()),
                operation.name
            ),
            range: to_proto::range(&line_index, operation.range),
            name: operation.name,
            signature: operation.signature,
            receiver_type: operation.receiver_type,
            required_access: operation.required_access,
            available_access: operation.available_access,
            why_required: operation.why_required,
            documentation: operation.documentation,
            effects: operation.effects,
            effect_facts: operation
                .effect_facts
                .into_iter()
                .map(|effect| lsp_ext::OwnershipOperationEffect {
                    kind: effect.kind,
                    summary: effect.summary,
                    certainty: effect.certainty,
                })
                .collect(),
            call_chain: operation.call_chain,
            alternatives: operation
                .alternatives
                .into_iter()
                .map(|alternative| lsp_ext::OwnershipOperationAlternative {
                    name: alternative.name,
                    signature: alternative.signature,
                    access: alternative.access,
                    behavior: alternative.behavior,
                    difference: alternative.difference,
                })
                .collect(),
            provenance: operation.provenance,
            truncated: operation.truncated,
        })
        .collect::<Vec<_>>();
    let mutation_requirement = ownership_mutation_requirement(
        selected_diagnostic,
        syntax.as_ref(),
        &line_index,
        &operations,
    );
    let source_context = syntax.as_ref().map(|syntax| {
        ownership_source_context(
            &params.text_document.uri,
            syntax,
            &line_index,
            request_range,
            &operations,
            &bindings,
        )
    });
    // Borrow-conflict diagnostics carry a richer source-level graph than the
    // sparse MIR event stream. In a file with several failing functions the
    // first unrelated MIR event can otherwise make every guided trace appear
    // to describe the same binding. Prefer the diagnostic-scoped graph.
    let value_trace = conflict_graph
        .as_ref()
        .map(ownership_conflict_value_trace)
        .filter(|trace| !trace.is_empty())
        .unwrap_or_else(|| ownership_value_trace(&events, &operations));

    Ok(lsp_ext::OwnershipModelResult {
        schema_version: tutorial.schema_version.max(12),
        target_triple: tutorial.target_triple.clone(),
        precision: if exact { "compiler_exact" } else { "estimated" }.to_owned(),
        status: if events.is_empty() && !has_tutorial_facts && selected_diagnostic.is_none() {
            "waiting_for_compiler"
        } else {
            "ready"
        }
        .to_owned(),
        truncated: response_truncated,
        source_hash: stable_source_hash(&source),
        selected_problem_id: selected_diagnostic.map(|diagnostic| {
            ownership_problem_from_diagnostic(diagnostic, &all_events, &tutorial).id
        }),
        selected_place: mutation_requirement
            .as_ref()
            .map(|requirement| requirement.target_place.clone())
            .or_else(|| selected.map(|event| event.place.clone()))
            .or_else(|| {
                selected_diagnostic
                    .and_then(|diagnostic| ownership_name_from_message(&diagnostic.message))
                    .map(str::to_owned)
            }),
        events,
        value_trace,
        repairs,
        bodies,
        bindings,
        loans,
        memory_graph,
        operations,
        mutation_requirement,
        conflict_graph,
        source_context,
        c_sketch,
    })
}

pub(crate) fn handle_ownership_problems(
    snap: GlobalStateSnapshot,
    params: TextDocumentIdentifier,
) -> anyhow::Result<lsp_ext::OwnershipProblemsResult> {
    let _p = tracing::info_span!("handle_ownership_problems").entered();
    let file_id = try_default!(from_proto::file_id(&snap, &params.uri)?);
    let source = snap.analysis.file_text(file_id)?;
    let diagnostics = ownership_diagnostics_for_file(&snap.ownership_diagnostics, file_id);
    let events = ownership_events_for_file(&snap.ownership_events, file_id);
    let tutorial =
        crate::diagnostics::ownership_tutorial_for_file(&snap.ownership_tutorial_models, file_id);
    let mut problems = diagnostics
        .iter()
        .map(|diagnostic| ownership_problem_from_diagnostic(diagnostic, &events, &tutorial))
        .collect::<Vec<_>>();

    // Legacy ownership-event transport can identify rejected post-move uses even when Cargo did
    // not retain the corresponding rustc diagnostic. Preserve that useful explanation while
    // deduplicating it against the richer diagnostic-backed problem above.
    for event in events.iter().filter(|event| event.kind == OwnershipEventKind::InvalidUse) {
        if problems.iter().any(|problem| ranges_touch(problem.primary_range, event.range)) {
            continue;
        }
        let related_events = ownership_events_for_binding(&events, event);
        let category =
            if related_events.iter().any(|related| related.kind == OwnershipEventKind::PartialMove)
            {
                "partial_move"
            } else {
                "use_after_move"
            };
        let mut related_ranges = related_events
            .iter()
            .map(|related| related.range)
            .filter(|range| *range != event.range)
            .collect::<Vec<_>>();
        sort_and_dedup_ranges(&mut related_ranges);
        problems.push(lsp_ext::OwnershipProblem {
            id: ownership_problem_id("E0382", &event.name, event.range),
            category: category.to_owned(),
            diagnostic_code: Some("E0382".to_owned()),
            message: format!("borrow of moved value: `{}`", event.name),
            binding_name: event.name.clone(),
            primary_range: event.range,
            binding_range: event.binding_range,
            related_ranges,
            related: Vec::new(),
            model_position: event.range.start,
            precision: if event.exact { "compiler_exact" } else { "estimated" }.to_owned(),
        });
    }
    problems.sort_by_key(|problem| (problem.primary_range.start, problem.primary_range.end));
    problems.dedup_by(|left, right| {
        left.category == right.category
            && left.binding_name == right.binding_name
            && left.primary_range == right.primary_range
    });

    Ok(lsp_ext::OwnershipProblemsResult {
        schema_version: tutorial.schema_version.max(1),
        status: if diagnostics.is_empty() && events.is_empty() && tutorial.schema_version == 0 {
            "waiting_for_compiler"
        } else {
            "ready"
        }
        .to_owned(),
        source_hash: stable_source_hash(&source),
        problems,
    })
}

pub(crate) fn handle_ownership_repair(
    snap: GlobalStateSnapshot,
    params: lsp_ext::OwnershipRepairParams,
) -> anyhow::Result<Option<lsp_ext::CodeAction>> {
    let _p = tracing::info_span!("handle_ownership_repair").entered();
    let request_range = Range::new(params.position, params.position);
    let file_range =
        try_default!(from_proto::file_range(&snap, &params.text_document, request_range,)?);
    let source = snap.analysis.file_text(file_range.file_id)?;
    if !ownership_repair_source_is_current(&source, &params.source_hash) {
        return Ok(None);
    }
    let diagnostics =
        ownership_diagnostics_for_file(&snap.ownership_diagnostics, file_range.file_id);
    let selected_diagnostic = diagnostics.iter().find(|diagnostic| {
        ranges_touch(diagnostic.range, request_range)
            || diagnostic.related.iter().any(|related| ranges_touch(related.range, request_range))
    });
    if let Some(action) =
        ownership_language_fixes_for_diagnostic(&snap, file_range.file_id, selected_diagnostic)
            .into_iter()
            .enumerate()
            .find_map(|(index, fix)| {
                (ownership_language_repair_id(index) == params.repair_id)
                    .then(|| fix.action.clone())
            })
    {
        return Ok(Some(action));
    }
    let events = ownership_events_for_file(&snap.ownership_events, file_range.file_id);
    let selected = events.iter().find(|event| {
        ranges_touch(event.range, request_range) || ranges_touch(event.binding_range, request_range)
    });
    Ok(ownership_fixes_at(&snap, file_range.file_id, request_range, selected)
        .into_iter()
        .enumerate()
        .find_map(|(index, fix)| {
            let strategy = fix.ownership_wrapper?;
            (ownership_repair_id(strategy, index) == params.repair_id).then(|| fix.action.clone())
        }))
}

pub(crate) fn handle_ownership_validate_repair(
    state: &mut GlobalState,
    params: lsp_ext::OwnershipRepairParams,
) -> anyhow::Result<lsp_ext::OwnershipRepairValidationResult> {
    let uri = params.text_document.uri;
    let vfs_path = from_proto::vfs_path(&uri)?;
    let snapshot = state.snapshot();
    let Some(file_id) = from_proto::file_id(&snapshot, &uri)? else {
        return Ok(lsp_ext::OwnershipRepairValidationResult {
            status: "unavailable".to_owned(),
            message: "The file is not part of the currently loaded Rust workspace.".to_owned(),
        });
    };
    let source = snapshot.analysis.file_text(file_id)?;
    if stable_source_hash(&source) != params.source_hash {
        return Ok(lsp_ext::OwnershipRepairValidationResult {
            status: "stale".to_owned(),
            message: "The source changed before compiler validation started.".to_owned(),
        });
    }
    drop(snapshot);
    let started = crate::handlers::notification::run_flycheck(state, vfs_path, true);
    Ok(lsp_ext::OwnershipRepairValidationResult {
        status: if started { "checking" } else { "unavailable" }.to_owned(),
        message: if started {
            "Compiler-checking ownership repair candidates for this package.".to_owned()
        } else {
            "No Cargo flycheck target was available for this file.".to_owned()
        },
    })
}

fn ownership_repair_source_is_current(source: &str, expected_hash: &str) -> bool {
    stable_source_hash(source) == expected_hash
}

fn ownership_problem_from_diagnostic(
    diagnostic: &crate::diagnostics::OwnershipDiagnostic,
    events: &[OwnershipEvent],
    tutorial: &crate::diagnostics::OwnershipTutorialModel,
) -> lsp_ext::OwnershipProblem {
    let diagnostic_name = ownership_name_from_message(&diagnostic.message).unwrap_or("value");
    let selected_event = events.iter().find(|event| {
        ranges_touch(event.range, diagnostic.range)
            || ownership_name_refers_to_binding(&event.name, diagnostic_name)
            || ownership_name_refers_to_binding(&event.place, diagnostic_name)
            || ownership_name_refers_to_binding(diagnostic_name, &event.name)
    });
    // E0594/E0596 diagnostics name the place that Rust tried to mutate (for example
    // `self.events`). An ownership event may be attached to its coarser root (`self`), but
    // replacing the diagnostic place with that root makes the learning UI explain the wrong
    // thing. Keep rustc's exact target for mutability problems and use events only for their
    // related ranges and binding metadata.
    let binding_name = if matches!(diagnostic.code.as_str(), "E0594" | "E0596") {
        diagnostic_name.to_owned()
    } else {
        selected_event.map(|event| event.name.as_str()).unwrap_or(diagnostic_name).to_owned()
    };
    let related_events =
        selected_event.map(|event| ownership_events_for_binding(events, event)).unwrap_or_default();
    let category = ownership_problem_category(&diagnostic.code, &related_events);
    let binding = tutorial.bindings.iter().find(|binding| {
        ownership_name_refers_to_binding(&binding_name, &binding.name)
            || ownership_name_refers_to_binding(&binding.name, &binding_name)
    });
    let binding_range = binding
        .map(|binding| binding.range)
        .or_else(|| selected_event.map(|event| event.binding_range))
        .unwrap_or(diagnostic.range);
    let mut related_ranges: Vec<_> =
        diagnostic.related.iter().map(|related| related.range).collect();
    related_ranges.extend(
        related_events.iter().map(|event| event.range).filter(|range| *range != diagnostic.range),
    );
    sort_and_dedup_ranges(&mut related_ranges);
    let exact = related_events.iter().any(|event| event.exact)
        || tutorial.schema_version >= 3 && binding.is_some();

    lsp_ext::OwnershipProblem {
        id: ownership_problem_id(&diagnostic.code, &binding_name, diagnostic.range),
        category: category.to_owned(),
        diagnostic_code: Some(diagnostic.code.clone()),
        message: diagnostic.message.clone(),
        binding_name,
        primary_range: diagnostic.range,
        binding_range,
        related_ranges,
        related: diagnostic
            .related
            .iter()
            .map(|related| lsp_ext::OwnershipProblemRelated {
                message: related.message.clone(),
                range: related.range,
            })
            .collect(),
        model_position: diagnostic.range.start,
        precision: if exact { "compiler_exact" } else { "estimated" }.to_owned(),
    }
}

fn ownership_problem_category(code: &str, events: &[&OwnershipEvent]) -> &'static str {
    match code {
        "E0382" if events.iter().any(|event| event.kind == OwnershipEventKind::PartialMove) => {
            "partial_move"
        }
        "E0382" => "use_after_move",
        "E0499" => "multiple_mutable_borrows",
        "E0502" => "mutable_while_shared",
        "E0503" => "use_while_mutably_borrowed",
        "E0505" => "move_while_borrowed",
        "E0506" => "assign_while_borrowed",
        "E0507" => "move_out_of_borrowed_content",
        "E0594" | "E0596" => "immutable_mutation",
        "E0106" => "missing_lifetime",
        "E0515" => "returning_local_reference",
        "E0597" => "borrowed_value_too_short",
        "E0716" => "temporary_dropped_while_borrowed",
        "E0277" => "trait_requirement",
        "E0308" => "type_mismatch",
        "E0599" => "method_or_trait_unavailable",
        "E0373" => "closure_may_outlive_borrow",
        "E0521" => "borrowed_data_escapes",
        "E0728" => "await_outside_async",
        "E0733" => "recursive_async_function",
        _ => "generic_borrow_error",
    }
}

fn ownership_mutation_requirement(
    diagnostic: Option<&crate::diagnostics::OwnershipDiagnostic>,
    file: Option<&syntax::SourceFile>,
    line_index: &LineIndex,
    operations: &[lsp_ext::OwnershipOperationInsight],
) -> Option<lsp_ext::OwnershipMutationRequirement> {
    let diagnostic = diagnostic?;
    if !matches!(diagnostic.code.as_str(), "E0594" | "E0596") {
        return None;
    }
    let diagnostic_target = ownership_name_from_message(&diagnostic.message)?.to_owned();
    let operation = operations
        .iter()
        .filter(|operation| {
            operation.required_access == "mutable_borrow"
                && ranges_touch(operation.range, diagnostic.range)
        })
        .min_by_key(|operation| {
            (
                operation.range.end.line.saturating_sub(operation.range.start.line),
                operation.range.end.character.saturating_sub(operation.range.start.character),
            )
        })
        .or_else(|| {
            // For an immutable local, rustc intentionally anchors E0596 at the `let` binding so
            // its primary suggestion can insert `mut`. The responsible method call is therefore
            // a later related operation rather than an overlapping span.
            operations
                .iter()
                .filter(|operation| operation.required_access == "mutable_borrow")
                .min_by_key(|operation| {
                    (
                        operation.range.start.line.abs_diff(diagnostic.range.start.line),
                        operation.range.start.character.abs_diff(diagnostic.range.start.character),
                    )
                })
        })?;
    let target_place = file
        .and_then(|file| {
            let operation_range = from_proto::text_range(line_index, operation.range).ok()?;
            let token = file.syntax().token_at_offset(operation_range.start()).right_biased()?;
            token
                .parent_ancestors()
                .filter_map(ast::MethodCallExpr::cast)
                .find(|call| call.syntax().text_range() == operation_range)?
                .receiver()
                .map(|receiver| bounded_source_label(&receiver.syntax().text().to_string()))
        })
        .unwrap_or(diagnostic_target);
    let receiver_type = operation.receiver_type.as_deref().unwrap_or_default();
    let diagnostic_message = diagnostic.message.to_ascii_lowercase();
    let (access_source, available_access, explanation) = if receiver_type.starts_with("Rc<")
        || receiver_type.starts_with("Arc<")
    {
        (
            format!("{target_place} through a shared-owner handle"),
            "shared_owner".to_owned(),
            format!(
                "`{target_place}` is reached through `{receiver_type}`. Shared ownership permits reading through the handle, but it does not provide the exclusive mutable access required by `{}`.",
                operation.name
            ),
        )
    } else if target_place == "self" || target_place.starts_with("self.") {
        let self_kind = file.and_then(|file| {
            let range = from_proto::text_range(line_index, diagnostic.range).ok()?;
            let token = file.syntax().token_at_offset(range.start()).right_biased()?;
            token
                .parent_ancestors()
                .find_map(ast::Fn::cast)?
                .param_list()?
                .self_param()
                .map(|parameter| parameter.kind())
        });
        match self_kind {
            Some(ast::SelfParamKind::Ref) => (
                "&self".to_owned(),
                "shared_borrow".to_owned(),
                format!(
                    "`{target_place}` is reached through `&self`. That permits shared reads, but it cannot create the exclusive mutable borrow required by `{}`.",
                    operation.name
                ),
            ),
            Some(ast::SelfParamKind::MutRef) if diagnostic_message.contains("behind a `&`") => (
                format!("a shared reference inside {target_place}"),
                "shared_borrow".to_owned(),
                format!(
                    "The method has `&mut self`, but `{target_place}` is still reached through a shared reference. That inner reference cannot provide the exclusive mutable borrow required by `{}`.",
                    operation.name
                ),
            ),
            Some(ast::SelfParamKind::MutRef) => (
                "&mut self".to_owned(),
                "mutable_receiver_with_blocked_path".to_owned(),
                diagnostic.message.clone(),
            ),
            Some(ast::SelfParamKind::Owned) => (
                "self".to_owned(),
                "owned_receiver_with_blocked_path".to_owned(),
                diagnostic.message.clone(),
            ),
            None => (
                "the path through self".to_owned(),
                "insufficient_mutable_access".to_owned(),
                diagnostic.message.clone(),
            ),
        }
    } else if diagnostic_message.contains("not declared as mutable") {
        let root = ownership_root_name(&target_place);
        (
            format!("immutable binding {root}"),
            "immutable_binding".to_owned(),
            format!(
                "`{root}` was not declared with `mut`, so `{target_place}` cannot provide the exclusive mutable borrow required by `{}`.",
                operation.name
            ),
        )
    } else if diagnostic_message.contains("behind a `&`") {
        (
            format!("shared reference to {target_place}"),
            "shared_borrow".to_owned(),
            format!(
                "`{target_place}` is reached through a shared reference, which cannot provide the exclusive mutable borrow required by `{}`.",
                operation.name
            ),
        )
    } else {
        (
            format!("the path to {target_place}"),
            "insufficient_mutable_access".to_owned(),
            diagnostic.message.clone(),
        )
    };

    Some(lsp_ext::OwnershipMutationRequirement {
        target_place,
        access_source,
        available_access,
        required_access: operation.required_access.clone(),
        operation_id: operation.id.clone(),
        operation_name: operation.name.clone(),
        explanation,
        provenance: "compiler_diagnostic_and_resolved_signature".to_owned(),
    })
}

fn ownership_source_context(
    uri: &Uri,
    file: &syntax::SourceFile,
    line_index: &LineIndex,
    request_range: Range,
    operations: &[lsp_ext::OwnershipOperationInsight],
    bindings: &[lsp_ext::OwnershipModelBinding],
) -> lsp_ext::OwnershipSourceContext {
    const MAX_BREADCRUMBS: usize = 8;
    const MAX_CALL_PATHS: usize = 12;
    const MAX_RELATED_TYPES: usize = 16;

    let file_label = uri
        .to_file_path()
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "Rust source file".to_owned());
    let mut breadcrumbs = vec![lsp_ext::OwnershipContextItem {
        kind: "file".to_owned(),
        label: file_label.clone(),
        range: None,
    }];
    if let Ok(offset) = from_proto::offset(line_index, request_range.start)
        && let Some(token) = file.syntax().token_at_offset(offset).right_biased()
    {
        let mut source_items = token
            .parent_ancestors()
            .filter_map(|node| {
                if let Some(function) = ast::Fn::cast(node.clone()) {
                    return function.name().map(|name| {
                        ("function", name.text().to_owned(), function.syntax().text_range())
                    });
                }
                if let Some(implementation) = ast::Impl::cast(node.clone()) {
                    return implementation.self_ty().map(|self_type| {
                        (
                            "implementation",
                            format!("impl {}", self_type.syntax().text()),
                            implementation.syntax().text_range(),
                        )
                    });
                }
                if let Some(trait_definition) = ast::Trait::cast(node.clone()) {
                    return trait_definition.name().map(|name| {
                        ("trait", name.text().to_owned(), trait_definition.syntax().text_range())
                    });
                }
                if let Some(module) = ast::Module::cast(node.clone()) {
                    return module.name().map(|name| {
                        ("module", name.text().to_owned(), module.syntax().text_range())
                    });
                }
                if let Some(structure) = ast::Struct::cast(node.clone()) {
                    return structure.name().map(|name| {
                        ("struct", name.text().to_owned(), structure.syntax().text_range())
                    });
                }
                ast::Enum::cast(node.clone()).and_then(|enumeration| {
                    enumeration.name().map(|name| {
                        ("enum", name.text().to_owned(), enumeration.syntax().text_range())
                    })
                })
            })
            .collect::<Vec<_>>();
        source_items.reverse();
        breadcrumbs.extend(source_items.into_iter().take(MAX_BREADCRUMBS - 1).map(
            |(kind, label, range)| lsp_ext::OwnershipContextItem {
                kind: kind.to_owned(),
                label,
                range: Some(to_proto::range(line_index, range)),
            },
        ));
    }

    let mut call_paths = operations
        .iter()
        .filter(|operation| !operation.call_chain.is_empty())
        .map(|operation| operation.call_chain.clone())
        .collect::<Vec<_>>();
    call_paths.sort();
    call_paths.dedup();
    let call_paths_truncated = call_paths.len() > MAX_CALL_PATHS;
    call_paths.truncate(MAX_CALL_PATHS);

    let mut related_types = bindings
        .iter()
        .map(|binding| binding.type_name.clone())
        .chain(operations.iter().filter_map(|operation| operation.receiver_type.clone()))
        .collect::<Vec<_>>();
    related_types.sort();
    related_types.dedup();
    let related_types_truncated = related_types.len() > MAX_RELATED_TYPES;
    related_types.truncate(MAX_RELATED_TYPES);

    lsp_ext::OwnershipSourceContext {
        file: file_label,
        breadcrumbs,
        call_paths,
        related_types,
        provenance: "source_syntax_resolved_calls_and_compiler_types".to_owned(),
        truncated: call_paths_truncated || related_types_truncated,
    }
}

#[derive(Clone)]
struct OwnershipSourcePlace {
    label: String,
    range: Range,
}

struct OwnershipBindingOwner {
    binding_range: Range,
    owner: OwnershipSourcePlace,
}

fn ownership_conflict_graph(
    diagnostic: &crate::diagnostics::OwnershipDiagnostic,
    file: &syntax::SourceFile,
    line_index: &LineIndex,
    tutorial: &crate::diagnostics::OwnershipTutorialModel,
    selected_body_id: Option<u64>,
) -> Option<lsp_ext::OwnershipConflictGraph> {
    if !matches!(diagnostic.code.as_str(), "E0499" | "E0502" | "E0505" | "E0506") {
        return None;
    }

    let borrow_origin = diagnostic
        .related
        .iter()
        .find(|related| {
            let message = related.message.to_ascii_lowercase();
            message.contains("borrow")
                && !message.contains("later")
                && (message.contains("here") || message.contains("occurs"))
        })
        .or_else(|| diagnostic.related.first());
    let last_use = diagnostic.related.iter().find(|related| {
        let message = related.message.to_ascii_lowercase();
        message.contains("borrow") && message.contains("later") && message.contains("used")
    });
    let origin_range = borrow_origin.map_or(diagnostic.range, |related| related.range);
    let origin_is_mutable = borrow_origin.is_some_and(|related| {
        let message = related.message.to_ascii_lowercase();
        message.contains("mutable borrow") && !message.contains("immutable borrow")
    });
    let origin = ownership_binding_at_range(file, line_index, origin_range);
    let borrower_name = origin
        .as_ref()
        .map(|(binding, _, _)| binding.clone())
        .unwrap_or_else(|| "temporary reference".to_owned());
    let attempted_place =
        ownership_name_from_message(&diagnostic.message).unwrap_or("value").to_owned();
    let origin_place = origin
        .as_ref()
        .and_then(|(_, _, initializer)| initializer.clone())
        .and_then(ownership_root_place)
        .map(|expression| OwnershipSourcePlace {
            label: bounded_source_label(&expression.syntax().text().to_string()),
            range: to_proto::range(line_index, expression.syntax().text_range()),
        });
    let attempted_root = ownership_root_name(&attempted_place);
    let borrowed_label = origin_place
        .as_ref()
        .filter(|place| {
            !attempted_place.starts_with('*') && ownership_root_name(&place.label) == attempted_root
        })
        .map(|place| place.label.clone())
        .unwrap_or_else(|| attempted_place.clone());
    let borrowed_range = origin_place.as_ref().map(|place| place.range).unwrap_or(origin_range);

    let reference_binding = attempted_place
        .strip_prefix('*')
        .and_then(|place| place.split(['.', '[']).next())
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let reference_owner = reference_binding
        .and_then(|binding| ownership_prior_binding_owner(file, line_index, origin_range, binding));
    let owner_place = reference_owner.as_ref().map(|binding| binding.owner.clone()).or_else(|| {
        let root = ownership_root_name(&borrowed_label);
        (root != borrowed_label)
            .then(|| OwnershipSourcePlace { label: root.to_owned(), range: borrowed_range })
    });

    let binding_for_name = |name: &str| {
        tutorial.bindings.iter().filter(|binding| binding.name == name).min_by_key(|binding| {
            (
                selected_body_id.is_some_and(|body_id| binding.body_id != body_id),
                binding.range.start.line.abs_diff(origin_range.start.line),
                binding.range.start.character.abs_diff(origin_range.start.character),
            )
        })
    };
    let binding_type = |name: &str| binding_for_name(name).map(|binding| binding.type_name.clone());
    let borrower_type = binding_type(&borrower_name);
    let reference_type = reference_binding.and_then(binding_type);
    let referent_type =
        reference_type.as_deref().and_then(reference_referent_type).map(str::to_owned);
    let borrower_range = origin.as_ref().map(|(_, range, _)| *range).or(Some(origin_range));

    let mut nodes = vec![lsp_ext::OwnershipConflictNode {
        id: "borrower".to_owned(),
        label: borrower_name.clone(),
        type_name: borrower_type,
        role: "borrower_reference".to_owned(),
        memory: "stack reference binding; it does not own the referenced value".to_owned(),
        range: borrower_range,
    }];
    if let Some(reference_binding) = reference_binding
        && reference_binding != borrower_name
    {
        nodes.push(lsp_ext::OwnershipConflictNode {
            id: "reference".to_owned(),
            label: reference_binding.to_owned(),
            type_name: reference_type,
            role: "reference_handle".to_owned(),
            memory: "stack reference handle pointing at the borrowed value".to_owned(),
            range: reference_owner
                .as_ref()
                .map(|binding| binding.binding_range)
                .or_else(|| binding_for_name(reference_binding).map(|binding| binding.range)),
        });
    }
    nodes.push(lsp_ext::OwnershipConflictNode {
        id: "referent".to_owned(),
        label: borrowed_label.clone(),
        type_name: referent_type,
        role: "borrowed_value".to_owned(),
        memory: "live borrowed value; its owner and storage remain intact".to_owned(),
        range: Some(borrowed_range),
    });
    if let Some(owner) = &owner_place
        && owner.label != borrowed_label
    {
        nodes.push(lsp_ext::OwnershipConflictNode {
            id: "owner".to_owned(),
            label: owner.label.clone(),
            type_name: binding_type(ownership_root_name(&owner.label)),
            role: "owner_path".to_owned(),
            memory: "owner or container path that keeps the borrowed storage alive".to_owned(),
            range: Some(owner.range),
        });
    }

    let mut edges = vec![lsp_ext::OwnershipConflictEdge {
        from: "borrower".to_owned(),
        to: "referent".to_owned(),
        kind: if origin_is_mutable { "borrows_mutable" } else { "borrows_shared" }.to_owned(),
        label: if origin_is_mutable {
            "holds a live exclusive borrow of"
        } else {
            "holds a live shared view into"
        }
        .to_owned(),
        provenance: "compiler_diagnostic".to_owned(),
    }];
    if reference_binding.is_some_and(|binding| binding != borrower_name) {
        edges.push(lsp_ext::OwnershipConflictEdge {
            from: "reference".to_owned(),
            to: "referent".to_owned(),
            kind: "points_to".to_owned(),
            label: "is the reference handle for".to_owned(),
            provenance: "compiler_type_and_source".to_owned(),
        });
    }
    if owner_place.as_ref().is_some_and(|owner| owner.label != borrowed_label) {
        edges.push(lsp_ext::OwnershipConflictEdge {
            from: "referent".to_owned(),
            to: "owner".to_owned(),
            kind: "stored_in".to_owned(),
            label: "is reached through storage owned by".to_owned(),
            provenance: "source_semantics".to_owned(),
        });
    }

    let (requested_access, operation, blocked_state) = match diagnostic.code.as_str() {
        "E0499" => (
            "second mutable borrow",
            "create another exclusive borrow of",
            "alive · exclusive access blocked",
        ),
        "E0502" => ("mutable borrow", "borrow mutably from", "alive · write access blocked"),
        "E0505" => ("move", "move ownership out of", "alive · move blocked"),
        "E0506" => ("replacement", "replace", "alive · replacement blocked"),
        _ => unreachable!(),
    };
    let end_range = last_use.map_or(diagnostic.range, |related| related.range);
    let node_states = |borrower_state: &str, referent_state: &str, owner_state: &str| {
        let mut states = vec![
            lsp_ext::OwnershipConflictNodeState {
                node_id: "borrower".to_owned(),
                state: borrower_state.to_owned(),
                explanation: "The reference is alive and usable.".to_owned(),
            },
            lsp_ext::OwnershipConflictNodeState {
                node_id: "referent".to_owned(),
                state: referent_state.to_owned(),
                explanation: "The value is alive; only incompatible access is restricted."
                    .to_owned(),
            },
        ];
        if nodes.iter().any(|node| node.id == "reference") {
            states.push(lsp_ext::OwnershipConflictNodeState {
                node_id: "reference".to_owned(),
                state: "alive · reference handle valid".to_owned(),
                explanation:
                    "The reference variable and the value it points to are different places."
                        .to_owned(),
            });
        }
        if nodes.iter().any(|node| node.id == "owner") {
            states.push(lsp_ext::OwnershipConflictNodeState {
                node_id: "owner".to_owned(),
                state: owner_state.to_owned(),
                explanation:
                    "The owner remains alive while access to the overlapping path is restricted."
                        .to_owned(),
            });
        }
        states
    };
    let snapshots = vec![
        lsp_ext::OwnershipConflictSnapshot {
            phase: "borrow_created".to_owned(),
            title: format!("`{borrower_name}` starts borrowing"),
            explanation: format!(
                "`{borrower_name}` now points into `{borrowed_label}`. The value remains alive, but incompatible writes or moves must wait."
            ),
            range: origin_range,
            states: node_states(
                if origin_is_mutable {
                    "alive · holds exclusive borrow"
                } else {
                    "alive · holds shared borrow"
                },
                blocked_state,
                "alive · overlapping path restricted",
            ),
        },
        lsp_ext::OwnershipConflictSnapshot {
            phase: "operation_rejected".to_owned(),
            title: format!("Rust rejects the {requested_access}"),
            explanation: format!(
                "This line tries to {operation} `{attempted_place}`, but `{borrower_name}` still needs its view of the existing value."
            ),
            range: diagnostic.range,
            states: node_states(
                "alive · still needed later",
                blocked_state,
                "alive · overlapping path restricted",
            ),
        },
        lsp_ext::OwnershipConflictSnapshot {
            phase: "borrow_ended".to_owned(),
            title: format!("`{borrower_name}` reaches its last use"),
            explanation: if last_use.is_some() {
                format!(
                    "After this use, the borrow held by `{borrower_name}` ends and `{borrowed_label}` can grant the previously blocked access again."
                )
            } else {
                "The compiler did not provide a source-level final-use span; the borrow ends at its compiler-calculated endpoint."
                    .to_owned()
            },
            range: end_range,
            states: node_states(
                "last use · borrow ends afterward",
                "alive · blocked access available afterward",
                "alive · normal access available afterward",
            ),
        },
    ];
    let title =
        format!("Cannot {operation} `{attempted_place}`: `{borrower_name}` still borrows from it");
    Some(lsp_ext::OwnershipConflictGraph {
        title,
        summary: format!(
            "`{borrower_name}` is the borrower; `{borrowed_label}` is the borrowed value. Borrowing does not make either one dead."
        ),
        requested_access: requested_access.to_owned(),
        nodes,
        edges,
        snapshots,
        provenance: if origin.is_some() {
            "compiler_diagnostic_and_source_semantics"
        } else {
            "compiler_diagnostic"
        }
        .to_owned(),
        truncated: false,
    })
}

fn ownership_binding_at_range(
    file: &syntax::SourceFile,
    line_index: &LineIndex,
    range: Range,
) -> Option<(String, Range, Option<ast::Expr>)> {
    let text_range = from_proto::text_range(line_index, range).ok()?;
    let token = file.syntax().token_at_offset(text_range.start()).right_biased()?;
    let let_statement = token.parent_ancestors().find_map(ast::LetStmt::cast)?;
    let identifier = let_statement.pat()?.syntax().descendants().find_map(ast::IdentPat::cast)?;
    let name = identifier.name()?.text().to_owned();
    let range = to_proto::range(line_index, identifier.syntax().text_range());
    Some((name, range, let_statement.initializer()))
}

fn ownership_prior_binding_owner(
    file: &syntax::SourceFile,
    line_index: &LineIndex,
    before: Range,
    binding_name: &str,
) -> Option<OwnershipBindingOwner> {
    let before = from_proto::offset(line_index, before.start).ok()?;
    let token = file.syntax().token_at_offset(before).right_biased()?;
    let scope = token
        .parent_ancestors()
        .find(|node| ast::Fn::can_cast(node.kind()) || ast::ClosureExpr::can_cast(node.kind()))?;
    let declaration = scope
        .descendants()
        .filter_map(ast::LetStmt::cast)
        .take(2048)
        .filter(|statement| statement.syntax().text_range().start() < before)
        .filter(|statement| {
            statement.pat().is_some_and(|pattern| {
                pattern
                    .syntax()
                    .descendants()
                    .filter_map(ast::IdentPat::cast)
                    .filter_map(|identifier| identifier.name())
                    .any(|name| name.text() == binding_name)
            })
        })
        .max_by_key(|statement| statement.syntax().text_range().start())?;
    let identifier = declaration
        .pat()?
        .syntax()
        .descendants()
        .filter_map(ast::IdentPat::cast)
        .find(|identifier| identifier.name().is_some_and(|name| name.text() == binding_name))?;
    let expression = ownership_root_place(declaration.initializer()?)?;
    Some(OwnershipBindingOwner {
        binding_range: to_proto::range(line_index, identifier.syntax().text_range()),
        owner: OwnershipSourcePlace {
            label: bounded_source_label(&expression.syntax().text().to_string()),
            range: to_proto::range(line_index, expression.syntax().text_range()),
        },
    })
}

fn ownership_root_place(mut expression: ast::Expr) -> Option<ast::Expr> {
    loop {
        expression = match expression {
            ast::Expr::RefExpr(reference) => reference.expr()?,
            ast::Expr::MethodCallExpr(call) => call.receiver()?,
            ast::Expr::IndexExpr(index) => index.syntax().children().find_map(ast::Expr::cast)?,
            ast::Expr::ParenExpr(parenthesized) => parenthesized.expr()?,
            ast::Expr::TryExpr(try_expression) => try_expression.expr()?,
            ast::Expr::AwaitExpr(await_expression) => await_expression.expr()?,
            ast::Expr::CastExpr(cast) => cast.expr()?,
            ast::Expr::PrefixExpr(prefix) => prefix.expr()?,
            ast::Expr::FieldExpr(_) | ast::Expr::PathExpr(_) => return Some(expression),
            _ => return Some(expression),
        };
    }
}

fn ownership_root_name(place: &str) -> &str {
    place
        .trim()
        .trim_start_matches('*')
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .split(['.', '[', '(', ' '])
        .next()
        .unwrap_or(place)
}

fn bounded_source_label(source: &str) -> String {
    let normalized = source.split_whitespace().join(" ");
    let mut label = normalized.chars().take(96).collect::<String>();
    if normalized.chars().count() > 96 {
        label.push('…');
    }
    label
}

fn reference_referent_type(type_name: &str) -> Option<&str> {
    type_name.strip_prefix("&mut ").or_else(|| type_name.strip_prefix('&')).map(str::trim)
}

fn ownership_events_for_binding<'a>(
    events: &'a [OwnershipEvent],
    selected: &OwnershipEvent,
) -> Vec<&'a OwnershipEvent> {
    events
        .iter()
        .filter(|event| {
            (selected.body_id == 0 || event.body_id == 0 || event.body_id == selected.body_id)
                && (ownership_name_refers_to_binding(&event.name, &selected.name)
                    || ownership_name_refers_to_binding(&event.place, &selected.name)
                    || ownership_name_refers_to_binding(&selected.name, &event.name))
        })
        .collect()
}

fn ownership_name_from_message(message: &str) -> Option<&str> {
    message.split('`').nth(1).filter(|name| !name.is_empty())
}

fn ownership_problem_id(code: &str, name: &str, range: Range) -> String {
    format!(
        "{code}-{name}-{}-{}-{}-{}",
        range.start.line, range.start.character, range.end.line, range.end.character
    )
}

fn sort_and_dedup_ranges(ranges: &mut Vec<Range>) {
    ranges.sort_by_key(|range| (range.start, range.end));
    ranges.dedup();
}

fn ownership_selection_context(
    selected: Option<&OwnershipEvent>,
    tutorial: &crate::diagnostics::OwnershipTutorialModel,
    request_range: Range,
) -> (Option<u64>, Option<String>) {
    let selected_binding = selected
        .and_then(|event| {
            tutorial.bindings.iter().find(|binding| {
                (event.body_id == 0 || binding.body_id == event.body_id)
                    && (ownership_name_refers_to_binding(&event.name, &binding.name)
                        || ownership_name_refers_to_binding(&event.place, &binding.name))
            })
        })
        .or_else(|| {
            tutorial.bindings.iter().find(|binding| ranges_touch(binding.range, request_range))
        });

    let body_id = selected
        .map(|event| event.body_id)
        .filter(|body_id| *body_id != 0)
        .or_else(|| selected_binding.map(|binding| binding.body_id));
    let name = selected_binding
        .map(|binding| binding.name.clone())
        .or_else(|| selected.map(|event| event.name.clone()));
    (body_id, name)
}

fn ownership_event_in_context(
    event: &OwnershipEvent,
    selected: Option<&OwnershipEvent>,
    selected_body_id: Option<u64>,
    selected_name: Option<&str>,
) -> bool {
    let Some(selected) = selected else {
        return selected_body_id
            .is_none_or(|body_id| event.body_id == 0 || event.body_id == body_id)
            && selected_name.is_none_or(|name| {
                ownership_name_refers_to_binding(&event.name, name)
                    || ownership_name_refers_to_binding(&event.place, name)
                    || ownership_name_refers_to_binding(name, &event.name)
            });
    };
    let Some(body_id) = selected_body_id else {
        return event.name == selected.name;
    };

    event.body_id == body_id
        || (event.body_id == 0
            && selected_name.is_some_and(|name| {
                ownership_name_refers_to_binding(&event.name, name)
                    || ownership_name_refers_to_binding(&event.place, name)
            }))
}

fn ownership_name_refers_to_binding(place: &str, binding: &str) -> bool {
    place == binding
        || place
            .strip_prefix(binding)
            .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('['))
}

fn ownership_loan_to_lsp(
    loan: &crate::diagnostics::OwnershipTutorialLoan,
) -> lsp_ext::OwnershipModelLoan {
    let point =
        |point: &crate::diagnostics::OwnershipTutorialLoanPoint| lsp_ext::OwnershipModelLoanPoint {
            basic_block: point.basic_block,
            statement_index: point.statement_index,
            range: point.range,
        };
    lsp_ext::OwnershipModelLoan {
        body_id: loan.body_id,
        loan_id: loan.loan_id,
        kind: loan.kind.clone(),
        name: loan.name.clone(),
        place: loan.place.clone(),
        reserve: point(&loan.reserve),
        activation: loan.activation.as_ref().map(point),
        live_points: loan.live_points.iter().take(512).map(point).collect(),
        end_points: loan.end_points.iter().take(64).map(point).collect(),
        truncated: loan.truncated || loan.live_points.len() > 512 || loan.end_points.len() > 64,
        provenance: "compiler_exact".to_owned(),
    }
}

fn ownership_repair_effects(strategy: OwnershipWrapperFix) -> lsp_ext::OwnershipModelRepairEffects {
    let (ownership, mutation, thread_safety, runtime_risk, cost) = match strategy {
        OwnershipWrapperFix::Rc => (
            "shared by non-atomic reference counting",
            "immutable unless the inner type provides mutation",
            "single-thread only",
            "cycles can leak",
            "clone/drop updates counters",
        ),
        OwnershipWrapperFix::Arc => (
            "shared by atomic reference counting",
            "immutable unless the inner type provides synchronization",
            "may cross threads when the inner type permits",
            "cycles can leak",
            "atomic counter operations",
        ),
        OwnershipWrapperFix::RefCell | OwnershipWrapperFix::RcRefCell => (
            "single owner or Rc-shared owner",
            "checked dynamically with borrow/borrow_mut",
            "single-thread only",
            "conflicting borrows panic",
            "runtime borrow-flag checks",
        ),
        OwnershipWrapperFix::Mutex | OwnershipWrapperFix::ArcMutex => (
            "single owner or Arc-shared owner",
            "exclusive mutation through a lock guard",
            "thread-safe when the inner type permits",
            "locking can block and poisoning must be handled",
            "lock acquisition on access",
        ),
        OwnershipWrapperFix::RwLock | OwnershipWrapperFix::ArcRwLock => (
            "single owner or Arc-shared owner",
            "multiple readers or one writer through lock guards",
            "thread-safe when the inner type permits",
            "locking can block and poisoning must be handled",
            "reader/writer lock acquisition",
        ),
        OwnershipWrapperFix::All => (
            "multiple compiler-validated rewrites",
            "depends on selected rewrite",
            "depends on selected rewrite",
            "review every candidate's runtime semantics",
            "depends on selected rewrite",
        ),
    };
    lsp_ext::OwnershipModelRepairEffects {
        ownership: ownership.to_owned(),
        mutation: mutation.to_owned(),
        thread_safety: thread_safety.to_owned(),
        runtime_risk: runtime_risk.to_owned(),
        cost: cost.to_owned(),
    }
}

fn ownership_repair_preview_graph(
    strategy: OwnershipWrapperFix,
    place: &str,
    range: Range,
    compiler_validated: bool,
) -> Option<lsp_ext::OwnershipModelMemoryGraph> {
    if matches!(strategy, OwnershipWrapperFix::All) {
        return None;
    }
    let provenance = if compiler_validated {
        "derived_from_compiler_validated_rewrite"
    } else {
        "conceptual_candidate"
    };
    let node = |id: &str,
                label: String,
                type_name: String,
                kind: &str,
                storage: &str|
     -> lsp_ext::OwnershipModelMemoryNode {
        lsp_ext::OwnershipModelMemoryNode {
            id: format!("preview-{id}"),
            body_id: 0,
            place: place.to_owned(),
            kind: kind.to_owned(),
            storage: storage.to_owned(),
            label,
            type_name,
            size: None,
            align: None,
            range: Some(range),
            state: "available_after_rewrite".to_owned(),
            provenance: provenance.to_owned(),
            physical_placement_note: "Counterfactual source-level topology; runtime addresses, counts, and lock/borrow state remain unknown."
                .to_owned(),
            truncated: false,
        }
    };
    let edge =
        |id: &str, source: &str, target: &str, relation: &str| lsp_ext::OwnershipModelMemoryEdge {
            id: format!("preview-{id}"),
            source: format!("preview-{source}"),
            target: format!("preview-{target}"),
            relation: relation.to_owned(),
            event_id: None,
            loan_id: None,
            range: Some(range),
            provenance: provenance.to_owned(),
            path_marker: Some("counterfactual_rewrite".to_owned()),
        };
    let (handle_type, shared, gate) = match strategy {
        OwnershipWrapperFix::Rc => ("Rc<T>", true, None),
        OwnershipWrapperFix::Arc => ("Arc<T>", true, None),
        OwnershipWrapperFix::RefCell => ("RefCell<T>", false, Some("runtime borrow flag")),
        OwnershipWrapperFix::Mutex => ("Mutex<T>", false, Some("exclusive lock gate")),
        OwnershipWrapperFix::RwLock => ("RwLock<T>", false, Some("reader/writer lock gate")),
        OwnershipWrapperFix::RcRefCell => ("Rc<RefCell<T>>", true, Some("runtime borrow flag")),
        OwnershipWrapperFix::ArcMutex => ("Arc<Mutex<T>>", true, Some("exclusive lock gate")),
        OwnershipWrapperFix::ArcRwLock => ("Arc<RwLock<T>>", true, Some("reader/writer lock gate")),
        OwnershipWrapperFix::All => unreachable!(),
    };
    let mut nodes = vec![node(
        "handle",
        format!("rewritten `{place}`"),
        handle_type.to_owned(),
        "binding",
        "stack",
    )];
    let mut edges = Vec::new();
    let parent = if shared {
        nodes.push(node(
            "allocation",
            "shared control block and allocation".to_owned(),
            "shared allocation; count is symbolic".to_owned(),
            "control_block",
            "heap",
        ));
        edges.push(edge("shares", "handle", "allocation", "shares_allocation"));
        "allocation"
    } else {
        "handle"
    };
    let parent = if let Some(gate) = gate {
        nodes.push(node(
            "gate",
            gate.to_owned(),
            "runtime access state is not sampled".to_owned(),
            if gate.contains("lock") { "lock_state" } else { "borrow_flag" },
            "inline",
        ));
        edges.push(edge("gate", parent, "gate", "guards_access"));
        "gate"
    } else {
        parent
    };
    nodes.push(node(
        "value",
        "inner value".to_owned(),
        "T".to_owned(),
        "inline_value",
        if shared { "heap" } else { "inline" },
    ));
    edges.push(edge("value", parent, "value", "contains"));
    Some(lsp_ext::OwnershipModelMemoryGraph {
        nodes,
        edges,
        snapshots: Vec::new(),
        access_paths: Vec::new(),
        truncated: false,
    })
}

fn ownership_c_sketch(
    selected_name: Option<&str>,
    bindings: &[lsp_ext::OwnershipModelBinding],
    events: &[lsp_ext::OwnershipModelEvent],
    repairs: &[lsp_ext::OwnershipModelRepair],
) -> Option<lsp_ext::OwnershipModelCSketch> {
    let name = selected_name?;
    let type_name = bindings.first().map(|binding| binding.type_name.as_str()).unwrap_or("T");
    let strategy = repairs.first().map(|repair| repair.strategy.as_str());
    let code = match strategy {
        Some("rc") | Some("arc") => format!(
            "// Operational sketch: shared ownership, not Rust ABI\nShared *{name} = shared_new(value);   // count = 1\nShared *alias = shared_retain({name}); // count += 1\nuse(alias->value);\nshared_release(alias);\nshared_release({name});                // free at count == 0"
        ),
        Some("ref_cell") | Some("rc_ref_cell") => format!(
            "// Operational sketch: dynamic borrow checking\nCell {name} = cell_new(value);\nif (!cell_try_borrow_mut(&{name})) panic_conflicting_borrow();\nmutate({name}.value);\ncell_release_mut(&{name});"
        ),
        Some("mutex") | Some("arc_mutex") | Some("rw_lock") | Some("arc_rw_lock") => format!(
            "// Operational sketch: synchronized interior mutation\nLock {name} = lock_new(value);\nGuard guard = lock_acquire(&{name});\nmutate(guard.value);\nlock_release(guard);"
        ),
        _ if events.iter().any(|event| event.kind == "move") => format!(
            "// Operational sketch: Rust move as ownership transfer\n{type_name} *owner = make_value();\n{type_name} *new_owner = owner;\nowner = NULL;              // old name must not be used\nuse(new_owner);\ndestroy(new_owner);"
        ),
        _ => format!(
            "// Operational sketch: a borrow is a temporary non-owning view\n{type_name} *{name} = make_value();\nconst {type_name} *view = {name};\nuse(view);                  // borrow ends after last use\ndestroy({name});"
        ),
    };
    Some(lsp_ext::OwnershipModelCSketch {
        title: format!("C-like intent sketch for `{name}`"),
        code,
        warning: "Explanatory pseudocode only: not Rust ABI-equivalent and not guaranteed to compile as C."
            .to_owned(),
        linked_event_ids: events.iter().map(|event| event.event_id.clone()).collect(),
        provenance: "conceptual".to_owned(),
    })
}

fn ownership_kind_code(kind: crate::diagnostics::OwnershipEventKind) -> &'static str {
    use crate::diagnostics::OwnershipEventKind;

    match kind {
        OwnershipEventKind::BorrowActivate => "borrow_activate",
        OwnershipEventKind::BorrowEnd => "borrow_end",
        OwnershipEventKind::BorrowMutable => "borrow_mutable",
        OwnershipEventKind::BorrowShared => "borrow_shared",
        OwnershipEventKind::Clone => "clone",
        OwnershipEventKind::Copy => "copy",
        OwnershipEventKind::Drop => "drop",
        OwnershipEventKind::InvalidUse => "invalid_use",
        OwnershipEventKind::LastUse => "last_use",
        OwnershipEventKind::Move => "move",
        OwnershipEventKind::PartialMove => "partial_move",
        OwnershipEventKind::Reinitialize => "reinitialize",
    }
}

fn ownership_value_trace(
    events: &[lsp_ext::OwnershipModelEvent],
    operations: &[lsp_ext::OwnershipOperationInsight],
) -> Vec<lsp_ext::OwnershipValueTraceStep> {
    const MAX_VALUE_TRACE_STEPS: usize = 24;
    let mut trace = Vec::new();
    if let Some(first) = events.first() {
        trace.push(lsp_ext::OwnershipValueTraceStep {
            id: format!("binding-{}", first.event_id),
            kind: "binding_introduced".to_owned(),
            range: first.binding_range,
            from_label: first.place.clone(),
            to_label: None,
            source_state: "available".to_owned(),
            destination_state: None,
            allocation_effect: "No allocation claim: this marks where the source binding is introduced."
                .to_owned(),
            explanation: format!(
                "`{}` is the name rustc tracks. A binding is a name for a value; it is not necessarily the heap allocation itself.",
                first.place
            ),
            provenance: "compiler_exact".to_owned(),
            control_flow: None,
        });
    }

    for event in events.iter().filter(|event| {
        matches!(
            event.kind.as_str(),
            "move"
                | "partial_move"
                | "clone"
                | "copy"
                | "borrow_shared"
                | "borrow_mutable"
                | "borrow_activate"
                | "borrow_end"
                | "invalid_use"
                | "reinitialize"
                | "drop"
        )
    }) {
        if trace.len() >= MAX_VALUE_TRACE_STEPS {
            break;
        }
        let destination = event.destination.as_ref();
        let (source_state, destination_state, allocation_effect, explanation) = match event
            .kind
            .as_str()
        {
            "move" | "partial_move" => {
                let target = destination
                    .map(|destination| destination.label.as_str())
                    .unwrap_or("the receiving operation");
                (
                    if event.kind == "partial_move" {
                        "partially moved"
                    } else {
                        "unavailable through this name"
                    },
                    Some("owns the transferred value"),
                    "Ownership changes hands; the underlying allocation is not copied by the move.",
                    format!(
                        "Ownership of `{}` flows to {target}. The old name no longer owns that value on this path.",
                        event.place
                    ),
                )
            }
            "copy" => (
                "still available",
                Some("receives a copied value"),
                "Copy duplicates the value representation; it does not create shared ownership.",
                format!(
                    "`{}` implements Copy, so using it leaves the source available.",
                    event.place
                ),
            ),
            "clone" => (
                "still available as one shared owner",
                Some("new handle to the same allocation"),
                "Clone duplicates the Rc/Arc handle and increments its symbolic strong count; the inner allocation is shared, not copied.",
                format!(
                    "`{}` remains usable while the destination becomes another owner of the same allocation.",
                    event.place
                ),
            ),
            "borrow_shared" => (
                "shared-borrowed",
                Some("temporary shared reference"),
                "No ownership or allocation moves; a temporary non-owning reference is created.",
                format!("A shared reference temporarily reads `{}`.", event.place),
            ),
            "borrow_mutable" | "borrow_activate" => (
                "mutably borrowed",
                Some("temporary exclusive reference"),
                "No ownership or allocation moves; access is exclusive for the loan's live range.",
                format!("An exclusive mutable reference temporarily accesses `{}`.", event.place),
            ),
            "borrow_end" => (
                "available after this point",
                None,
                "No allocation changes; the compiler loan ends.",
                format!("The temporary borrow of `{}` is no longer live.", event.place),
            ),
            "invalid_use" => (
                "unavailable",
                None,
                "No runtime operation occurs because rustc rejects the program.",
                format!(
                    "This use asks `{}` for a value that this name no longer owns.",
                    event.place
                ),
            ),
            "reinitialize" => (
                "available with a new value",
                None,
                "A new value is assigned to the binding; this does not restore the old moved value.",
                format!("`{}` receives a new value and becomes usable again.", event.place),
            ),
            "drop" => (
                "dropped",
                None,
                "The owned value is destroyed; owned resources are released according to Drop.",
                format!("`{}` reaches its drop point.", event.place),
            ),
            _ => continue,
        };
        trace.push(lsp_ext::OwnershipValueTraceStep {
            id: event.event_id.clone(),
            kind: event.kind.clone(),
            range: event.range,
            from_label: event.place.clone(),
            to_label: destination.map(|destination| destination.label.clone()),
            source_state: source_state.to_owned(),
            destination_state: destination_state.map(str::to_owned),
            allocation_effect: allocation_effect.to_owned(),
            explanation,
            provenance: "compiler_exact".to_owned(),
            control_flow: Some(format!(
                "MIR bb{}, statement {}",
                event.basic_block, event.statement_index
            )),
        });
    }

    for operation in operations.iter().filter(|operation| operation.name == "clone") {
        if trace.len() >= MAX_VALUE_TRACE_STEPS {
            break;
        }
        let receiver = operation.receiver_type.as_deref().unwrap_or("the receiver");
        let shared_handle = receiver.contains("Rc<") || receiver.contains("Arc<");
        trace.push(lsp_ext::OwnershipValueTraceStep {
            id: format!("clone-{}", operation.id),
            kind: "clone".to_owned(),
            range: operation.range,
            from_label: receiver.to_owned(),
            to_label: Some("the new clone result".to_owned()),
            source_state: "still available".to_owned(),
            destination_state: Some(if shared_handle {
                "new handle to the same allocation"
            } else {
                "independent cloned value"
            }
            .to_owned()),
            allocation_effect: if shared_handle {
                "Rc::clone/Arc::clone keeps the allocation and increases the symbolic strong count by one."
            } else {
                "Clone behavior and allocation cost come from this type's Clone implementation."
            }
            .to_owned(),
            explanation: format!("Resolved `{receiver}::clone` returns another usable value."),
            provenance: operation.provenance.clone(),
            control_flow: None,
        });
    }
    trace
}

fn ownership_conflict_value_trace(
    graph: &lsp_ext::OwnershipConflictGraph,
) -> Vec<lsp_ext::OwnershipValueTraceStep> {
    let node = |id: &str| graph.nodes.iter().find(|node| node.id == id);
    let Some(borrower) = node("borrower") else { return Vec::new() };
    let Some(referent) = node("referent") else { return Vec::new() };
    let owner = node("owner");
    let borrow_kind = graph
        .edges
        .iter()
        .find(|edge| edge.from == "borrower" && edge.to == "referent")
        .map(|edge| edge.kind.as_str())
        .unwrap_or("borrows_shared");

    graph
        .snapshots
        .iter()
        .enumerate()
        .map(|(index, snapshot)| {
            let state = |node_id: &str| {
                snapshot
                    .states
                    .iter()
                    .find(|state| state.node_id == node_id)
                    .map(|state| state.state.clone())
            };
            let (kind, from_label, to_label, allocation_effect) = match snapshot.phase.as_str() {
                "borrow_created" => (
                    if borrow_kind == "borrows_mutable" {
                        "borrow_mutable"
                    } else {
                        "borrow_shared"
                    },
                    borrower.label.clone(),
                    Some(referent.label.clone()),
                    "A reference is created. Ownership and the underlying allocation stay where they are.",
                ),
                "operation_rejected" => (
                    "invalid_use",
                    referent.label.clone(),
                    Some(borrower.label.clone()),
                    "No write, move, or allocation occurs because rustc rejects the incompatible access.",
                ),
                "borrow_ended" => (
                    "borrow_end",
                    borrower.label.clone(),
                    Some(referent.label.clone()),
                    "The reference reaches its last use. Ownership is unchanged and the blocked access becomes available afterward.",
                ),
                _ => (
                    "state",
                    borrower.label.clone(),
                    Some(referent.label.clone()),
                    "No ownership or allocation change is implied by this compiler snapshot.",
                ),
            };
            let destination_state = match snapshot.phase.as_str() {
                "operation_rejected" => state("borrower"),
                _ => state("referent"),
            };
            lsp_ext::OwnershipValueTraceStep {
                id: format!("conflict-{index}-{}", snapshot.phase),
                kind: kind.to_owned(),
                range: snapshot.range,
                from_label,
                to_label,
                source_state: match snapshot.phase.as_str() {
                    "operation_rejected" => state("referent"),
                    _ => state("borrower"),
                }
                .unwrap_or_else(|| "alive".to_owned()),
                destination_state,
                allocation_effect: allocation_effect.to_owned(),
                explanation: if let Some(owner) = owner {
                    format!(
                        "{} `{}` remains the owner path throughout this step.",
                        snapshot.explanation, owner.label
                    )
                } else {
                    snapshot.explanation.clone()
                },
                provenance: graph.provenance.clone(),
                control_flow: None,
            }
        })
        .collect()
}

fn ownership_state_code(state: OwnershipState) -> &'static str {
    match state {
        OwnershipState::Available => "available",
        OwnershipState::Dropped => "dropped",
        OwnershipState::Moved => "moved",
        OwnershipState::MutablyBorrowed => "mutably_borrowed",
        OwnershipState::PartiallyMoved => "partially_moved",
        OwnershipState::SharedBorrowed => "shared_borrowed",
    }
}

fn ownership_wrapper_code(strategy: OwnershipWrapperFix) -> &'static str {
    match strategy {
        OwnershipWrapperFix::Arc => "arc",
        OwnershipWrapperFix::ArcMutex => "arc_mutex",
        OwnershipWrapperFix::ArcRwLock => "arc_rw_lock",
        OwnershipWrapperFix::Mutex => "mutex",
        OwnershipWrapperFix::Rc => "rc",
        OwnershipWrapperFix::RefCell => "ref_cell",
        OwnershipWrapperFix::RcRefCell => "rc_ref_cell",
        OwnershipWrapperFix::RwLock => "rw_lock",
        OwnershipWrapperFix::All => "all",
    }
}

fn ownership_repair_id(strategy: OwnershipWrapperFix, index: usize) -> String {
    format!("{}-{index}", ownership_wrapper_code(strategy))
}

fn ownership_alternatives(
    snap: &GlobalStateSnapshot,
    uri: &Uri,
    file_id: FileId,
    line_index: &LineIndex,
    request_range: Range,
    selected: Option<&OwnershipEvent>,
) -> anyhow::Result<Option<String>> {
    let fixes = ownership_fixes_at(snap, file_id, request_range, selected);
    if fixes.is_empty() {
        return Ok(None);
    }

    let source = snap.analysis.file_text(file_id)?;
    let mut rendered = String::from("**Ownership alternatives (compiler validated)**");
    let mut seen = Vec::new();
    let mut alternatives = 0;
    for fix in fixes {
        let Some(strategy) = fix.ownership_wrapper else { continue };
        if strategy == OwnershipWrapperFix::All || seen.contains(&strategy) {
            continue;
        }
        let Some(diff) = ownership_fix_diff(&source, line_index, uri, fix) else { continue };
        seen.push(strategy);
        alternatives += 1;
        let title = fix.action.title.trim_end_matches(" (compiler validated)");
        format_to!(
            rendered,
            "\n\n**{title}**  \n_{}_\n\n```diff\n{diff}\n```",
            strategy.runtime_semantics(),
        );
    }
    if alternatives == 0 {
        return Ok(None);
    }
    rendered.push_str("\n\nPress `Ctrl+.` on the highlighted value to apply an alternative.");
    Ok(Some(rendered))
}

fn ownership_fixes_at<'a>(
    snap: &'a GlobalStateSnapshot,
    file_id: FileId,
    request_range: Range,
    selected: Option<&OwnershipEvent>,
) -> Vec<&'a Fix> {
    let mut fixes = snap
        .check_fixes
        .iter()
        .flat_map(|flycheck| flycheck.values())
        .filter_map(|package| package.get(&file_id))
        .flatten()
        .filter(|fix| {
            fix.ownership_wrapper.is_some_and(|strategy| strategy != OwnershipWrapperFix::All)
                && fix.ranges.iter().copied().any(|range| {
                    ranges_touch(range, request_range)
                        || selected.is_some_and(|event| {
                            ranges_touch(range, event.range)
                                || ranges_touch(range, event.binding_range)
                        })
                })
        })
        .collect::<Vec<_>>();
    fixes.sort_by_key(|fix| fix.ownership_wrapper.map(OwnershipWrapperFix::preview_order));
    fixes
}

fn ownership_language_fixes_for_diagnostic<'a>(
    snap: &'a GlobalStateSnapshot,
    file_id: FileId,
    diagnostic: Option<&crate::diagnostics::OwnershipDiagnostic>,
) -> Vec<&'a Fix> {
    let Some(diagnostic) = diagnostic else {
        return Vec::new();
    };
    let mut fixes = snap
        .check_fixes
        .iter()
        .flat_map(|flycheck| flycheck.values())
        .filter_map(|package| package.get(&file_id))
        .flatten()
        .filter(|fix| {
            fix.ownership_wrapper.is_none()
                && fix.action.edit.is_some()
                && fix.ranges.iter().copied().any(|range| {
                    ranges_touch(range, diagnostic.range)
                        || diagnostic
                            .related
                            .iter()
                            .any(|related| ranges_touch(range, related.range))
                })
        })
        .collect::<Vec<_>>();
    fixes.sort_by(|left, right| {
        right
            .action
            .is_preferred
            .unwrap_or(false)
            .cmp(&left.action.is_preferred.unwrap_or(false))
            .then_with(|| left.action.title.cmp(&right.action.title))
    });
    fixes
}

fn ownership_language_repair_id(index: usize) -> String {
    format!("language-fix-{index}")
}

fn ownership_language_repair_effects(title: &str) -> lsp_ext::OwnershipModelRepairEffects {
    let title = title.to_ascii_lowercase();
    if title.contains("mutable reference") || title.contains("mut") {
        lsp_ext::OwnershipModelRepairEffects {
            ownership: "keeps ordinary unique ownership".to_owned(),
            mutation: "grants compile-time checked exclusive mutable access".to_owned(),
            thread_safety: "unchanged; callers must satisfy the existing Send/Sync constraints"
                .to_owned(),
            runtime_risk: "none added; borrow conflicts remain compile-time errors".to_owned(),
            cost: "zero-cost access change; callers may need mutable bindings".to_owned(),
        }
    } else {
        lsp_ext::OwnershipModelRepairEffects {
            ownership: "follows the compiler-proposed source change".to_owned(),
            mutation: "depends on the compiler suggestion".to_owned(),
            thread_safety: "must be rechecked at affected callers".to_owned(),
            runtime_risk: "no wrapper runtime behavior is assumed".to_owned(),
            cost: "validated by a fresh Cargo check before the result is claimed".to_owned(),
        }
    }
}

fn ownership_candidate_fixes(
    snap: &GlobalStateSnapshot,
    file_id: FileId,
    file_range: TextRange,
    request_range: Range,
) -> anyhow::Result<Vec<Fix>> {
    let source_root = snap.analysis.source_root_id(file_id)?;
    let assists_config = snap.config.assist(Some(source_root));
    let assists = snap.analysis.assists_with_fixes(
        &assists_config,
        &snap.config.diagnostic_fixes(Some(source_root)),
        AssistResolveStrategy::All,
        FileRange { file_id, range: file_range },
    )?;
    let client_commands = snap.config.client_commands();
    let mut fixes = Vec::new();
    for assist in assists {
        let title = assist.label.to_string();
        let Some(strategy) = heuristic_ownership_strategy(&title) else {
            continue;
        };
        let action = to_proto::code_action(snap, &client_commands, assist, None)?;
        fixes.push(Fix {
            ranges: smallvec::smallvec![request_range],
            action,
            ownership_wrapper: Some(strategy),
        });
    }
    fixes.sort_by_key(|fix| fix.ownership_wrapper.map(OwnershipWrapperFix::preview_order));
    Ok(fixes)
}

fn heuristic_ownership_strategy(title: &str) -> Option<OwnershipWrapperFix> {
    match title {
        "Use Rc for shared ownership (unvalidated)" => Some(OwnershipWrapperFix::Rc),
        "Use RefCell for interior mutability (unvalidated)" => Some(OwnershipWrapperFix::RefCell),
        "Use Rc<RefCell<_>> for shared mutable ownership (unvalidated)" => {
            Some(OwnershipWrapperFix::RcRefCell)
        }
        _ => None,
    }
}

fn ranges_touch(left: Range, right: Range) -> bool {
    left.start <= right.end && right.start <= left.end
}

#[derive(Debug)]
struct OwnershipPreviewEdit {
    range: TextRange,
    replacement: String,
    start_line: u32,
    end_line: u32,
}

fn ownership_fix_diff(
    source: &str,
    line_index: &LineIndex,
    uri: &Uri,
    fix: &Fix,
) -> Option<String> {
    let workspace_edit = fix.action.edit.as_ref()?;
    let source_edits = if let Some(changes) = workspace_edit.changes.as_ref() {
        changes.get(uri)?.iter().map(|edit| (edit.range, edit.new_text.clone())).collect::<Vec<_>>()
    } else {
        workspace_edit
            .document_changes
            .as_ref()?
            .iter()
            .filter_map(|change| match change {
                lsp_ext::SnippetDocumentChangeOperation::Edit(edit)
                    if &edit.text_document.text_document_identifier.uri == uri =>
                {
                    Some(edit.edits.iter().map(|edit| (edit.range, edit.new_text.clone())))
                }
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>()
    };
    let mut edits = source_edits
        .into_iter()
        .map(|(edit_range, replacement)| {
            let range = from_proto::text_range(line_index, edit_range).ok()?;
            let end_line =
                if edit_range.end.character == 0 && edit_range.end.line > edit_range.start.line {
                    edit_range.end.line - 1
                } else {
                    edit_range.end.line
                };
            Some(OwnershipPreviewEdit {
                range,
                replacement,
                start_line: edit_range.start.line,
                end_line,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    edits.sort_by_key(|edit| (edit.range.start(), edit.range.end()));
    if edits.windows(2).any(|pair| {
        pair[0].range.end() > pair[1].range.start()
            || pair[0].range.is_empty()
                && pair[1].range.is_empty()
                && pair[0].range.start() == pair[1].range.start()
    }) {
        return None;
    }

    let mut groups: Vec<Vec<&OwnershipPreviewEdit>> = Vec::new();
    for edit in &edits {
        if let Some(group) = groups.last_mut()
            && group.iter().map(|edit| edit.end_line).max().unwrap_or(0) + 1 >= edit.start_line
        {
            group.push(edit);
        } else {
            groups.push(vec![edit]);
        }
    }

    const MAX_DIFF_LINES: usize = 24;
    let mut lines = groups
        .iter()
        .filter_map(|group| ownership_diff_hunk(source, group))
        .flatten()
        .collect::<Vec<_>>();
    if lines.len() > MAX_DIFF_LINES {
        lines.truncate(MAX_DIFF_LINES - 1);
        lines.push(" ... preview truncated; use Ctrl+. to apply the complete rewrite ...".into());
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn ownership_diff_hunk(source: &str, edits: &[&OwnershipPreviewEdit]) -> Option<Vec<String>> {
    let first = edits.first()?;
    let start = edits.iter().map(|edit| usize::from(edit.range.start())).min()?;
    let end = edits.iter().map(|edit| usize::from(edit.range.end())).max()?;
    let end_anchor =
        if end > start { source.get(..end)?.char_indices().next_back()?.0 } else { end };
    let core_start = source.get(..start)?.rfind('\n').map_or(0, |offset| offset + 1);
    let core_end =
        source.get(end_anchor..)?.find('\n').map_or(source.len(), |offset| end_anchor + offset);
    let old = source.get(core_start..core_end)?;
    let mut patched = old.to_owned();
    for edit in edits.iter().rev() {
        let edit_start = usize::from(edit.range.start()).checked_sub(core_start)?;
        let edit_end = usize::from(edit.range.end()).checked_sub(core_start)?;
        patched.get(edit_start..edit_end)?;
        patched.replace_range(edit_start..edit_end, &edit.replacement);
    }
    if old == patched {
        return None;
    }

    let mut lines = vec![format!("@@ line {} @@", first.start_line + 1)];
    if core_start > 0 {
        let context_end = core_start - 1;
        let context_start = source.get(..context_end)?.rfind('\n').map_or(0, |offset| offset + 1);
        lines.push(format!(" {}", source.get(context_start..context_end)?));
    }
    lines.extend(old.split('\n').map(|line| format!("-{line}")));
    lines.extend(patched.split('\n').map(|line| format!("+{line}")));
    if core_end < source.len() {
        let context_start = core_end + 1;
        let context_end = source
            .get(context_start..)?
            .find('\n')
            .map_or(source.len(), |offset| context_start + offset);
        lines.push(format!(" {}", source.get(context_start..context_end)?));
    }
    Some(lines)
}

fn ownership_timeline(
    events: &[crate::diagnostics::OwnershipEvent],
    selected: &crate::diagnostics::OwnershipEvent,
) -> String {
    let timeline = events
        .iter()
        .filter(|event| event.name == selected.name)
        .map(|event| {
            let detail =
                event.detail.as_deref().map(|detail| format!(" — {detail}")).unwrap_or_default();
            format!("- line {}: **{}**{detail}", event.range.start.line + 1, event.kind.label(),)
        })
        .join("\n");
    format!("**Ownership timeline for `{}` (compiler MIR)**\n\n{timeline}", selected.name)
}

pub(crate) fn handle_prepare_rename(
    snap: GlobalStateSnapshot,
    params: lsp_types::PrepareRenameParams,
) -> anyhow::Result<Option<PrepareRenameResult>> {
    let _p = tracing::info_span!("handle_prepare_rename").entered();
    let position =
        try_default!(from_proto::file_position(&snap, &params.text_document_position_params)?);

    let change = snap.analysis.prepare_rename(position)?.map_err(to_proto::rename_error)?;

    let line_index = snap.file_line_index(position.file_id)?;
    let range = to_proto::range(&line_index, change.range);
    Ok(Some(PrepareRenameResult::Range(range)))
}

pub(crate) fn handle_rename(
    snap: GlobalStateSnapshot,
    params: RenameParams,
) -> anyhow::Result<Option<WorkspaceEdit>> {
    let _p = tracing::info_span!("handle_rename").entered();
    let position =
        try_default!(from_proto::file_position(&snap, &params.text_document_position_params)?);

    let source_root = snap.analysis.source_root_id(position.file_id).ok();
    let config = snap.config.rename(source_root);
    let mut change = snap
        .analysis
        .rename(position, &params.new_name, &config)?
        .map_err(to_proto::rename_error)?;

    // this is kind of a hack to prevent double edits from happening when moving files
    // When a module gets renamed by renaming the mod declaration this causes the file to move
    // which in turn will trigger a WillRenameFiles request to the server for which we reply with a
    // a second identical set of renames, the client will then apply both edits causing incorrect edits
    // with this we only emit source_file_edits in the WillRenameFiles response which will do the rename instead
    // See https://github.com/microsoft/vscode-languageserver-node/issues/752 for more info
    if !change.file_system_edits.is_empty() && snap.config.will_rename() {
        change.source_file_edits.clear();
    }

    let workspace_edit = to_proto::workspace_edit(&snap, change)?;

    if let Some(changes) = workspace_edit.document_changes.as_ref() {
        for change in changes {
            resource_ops_supported(&snap.config, change)?;
        }
    }

    Ok(Some(workspace_edit))
}

pub(crate) fn handle_references(
    snap: GlobalStateSnapshot,
    params: lsp_types::ReferenceParams,
) -> anyhow::Result<Option<Vec<Location>>> {
    let _p = tracing::info_span!("handle_references").entered();
    let position =
        try_default!(from_proto::file_position(&snap, &params.text_document_position_params)?);

    let exclude_imports = snap.config.find_all_refs_exclude_imports();
    let exclude_tests = snap.config.find_all_refs_exclude_tests();
    let Some(refs) = snap.analysis.find_all_refs(
        position,
        &FindAllRefsConfig {
            search_scope: None,
            ra_fixture: snap.config.ra_fixture(snap.minicore()),
            exclude_imports,
            exclude_tests,
        },
    )?
    else {
        return Ok(None);
    };

    let include_declaration = params.context.include_declaration;
    let locations = refs
        .into_iter()
        .flat_map(|refs| {
            let decl = if include_declaration {
                refs.declaration.map(|decl| FileRange {
                    file_id: decl.nav.file_id,
                    range: decl.nav.focus_or_full_range(),
                })
            } else {
                None
            };
            refs.references
                .into_iter()
                .flat_map(|(file_id, refs)| {
                    refs.into_iter().map(move |(range, _)| FileRange { file_id, range })
                })
                .chain(decl)
        })
        .unique()
        .filter_map(|frange| to_proto::location(&snap, frange).ok())
        .collect();

    Ok(Some(locations))
}

pub(crate) fn handle_formatting(
    snap: GlobalStateSnapshot,
    params: lsp_types::DocumentFormattingParams,
) -> anyhow::Result<Option<Vec<lsp_types::TextEdit>>> {
    let _p = tracing::info_span!("handle_formatting").entered();

    run_rustfmt(&snap, &params.text_document, None)
}

pub(crate) fn handle_range_formatting(
    snap: GlobalStateSnapshot,
    params: lsp_types::DocumentRangeFormattingParams,
) -> anyhow::Result<Option<Vec<lsp_types::TextEdit>>> {
    let _p = tracing::info_span!("handle_range_formatting").entered();

    run_rustfmt(&snap, &params.text_document, Some(params.range))
}

pub(crate) fn handle_code_action(
    snap: GlobalStateSnapshot,
    params: lsp_types::CodeActionParams,
) -> anyhow::Result<Option<Vec<lsp_ext::CodeAction>>> {
    let _p = tracing::info_span!("handle_code_action").entered();

    if !snap.config.code_action_literals() {
        // We intentionally don't support command-based actions, as those either
        // require either custom client-code or server-initiated edits. Server
        // initiated edits break causality, so we avoid those.
        return Ok(None);
    }

    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let line_index = snap.file_line_index(file_id)?;
    let frange = try_default!(from_proto::file_range(&snap, &params.text_document, params.range)?);
    let source_root = snap.analysis.source_root_id(file_id)?;

    let mut assists_config = snap.config.assist(Some(source_root));
    assists_config.allowed = params
        .context
        .only
        .clone()
        .map(|it| it.into_iter().filter_map(from_proto::assist_kind).collect());

    let mut res: Vec<lsp_ext::CodeAction> = Vec::new();

    let code_action_resolve_cap = snap.config.code_action_resolve();
    let resolve = if code_action_resolve_cap {
        AssistResolveStrategy::None
    } else {
        AssistResolveStrategy::All
    };
    let assists = snap.analysis.assists_with_fixes(
        &assists_config,
        &snap.config.diagnostic_fixes(Some(source_root)),
        resolve,
        frange,
    )?;
    let client_commands = snap.config.client_commands();
    for (index, assist) in assists.into_iter().enumerate() {
        let resolve_data = if code_action_resolve_cap {
            Some((index, params.clone(), snap.file_version(file_id)))
        } else {
            None
        };
        let code_action = to_proto::code_action(&snap, &client_commands, assist, resolve_data)?;

        // Check if the client supports the necessary `ResourceOperation`s.
        let changes = code_action.edit.as_ref().and_then(|it| it.document_changes.as_ref());
        if let Some(changes) = changes {
            for change in changes {
                if let lsp_ext::SnippetDocumentChangeOperation::Change(change) = change {
                    resource_ops_supported(&snap.config, change)?
                }
            }
        }

        res.push(code_action)
    }

    // Fixes from `cargo check`.
    let fixes = snap
        .check_fixes
        .iter()
        .flat_map(|it| it.values())
        .filter_map(|it| it.get(&frange.file_id))
        .flatten()
        .filter(|fix| {
            fix.ranges
                .iter()
                .copied()
                .filter_map(|range| from_proto::text_range(&line_index, range).ok())
                .any(|fix_range| fix_range.intersect(frange.range).is_some())
        })
        .collect::<Vec<_>>();
    let validated_wrappers = fixes
        .iter()
        .filter_map(|fix| fix.ownership_wrapper)
        .filter(|fix| *fix != crate::diagnostics::OwnershipWrapperFix::All)
        .collect::<Vec<_>>();
    res.retain(|action| {
        let heuristic = match action.title.as_str() {
            "Use Rc for shared ownership (unvalidated)" => {
                Some(crate::diagnostics::OwnershipWrapperFix::Rc)
            }
            "Use RefCell for interior mutability (unvalidated)" => {
                Some(crate::diagnostics::OwnershipWrapperFix::RefCell)
            }
            "Use Rc<RefCell<_>> for shared mutable ownership (unvalidated)" => {
                Some(crate::diagnostics::OwnershipWrapperFix::RcRefCell)
            }
            _ => None,
        };
        !heuristic.is_some_and(|heuristic| validated_wrappers.contains(&heuristic))
    });
    for fix in fixes {
        // FIXME: this mapping is awkward and shouldn't exist. Refactor
        // `snap.check_fixes` to not convert to LSP prematurely.
        res.push(fix.action.clone());
    }

    Ok(Some(res))
}

pub(crate) fn handle_code_action_resolve(
    snap: GlobalStateSnapshot,
    mut code_action: lsp_ext::CodeAction,
) -> anyhow::Result<lsp_ext::CodeAction> {
    let _p = tracing::info_span!("handle_code_action_resolve").entered();
    let Some(params) = code_action.data.take() else {
        return Ok(code_action);
    };

    let file_id = from_proto::file_id(&snap, &params.code_action_params.text_document.uri)?
        .expect("we never provide code actions for excluded files");
    if snap.file_version(file_id) != params.version {
        return Err(invalid_params_error("stale code action".to_owned()).into());
    }
    let line_index = snap.file_line_index(file_id)?;
    let range = from_proto::text_range(&line_index, params.code_action_params.range)?;
    let frange = FileRange { file_id, range };
    let source_root = snap.analysis.source_root_id(file_id)?;

    let mut assists_config = snap.config.assist(Some(source_root));
    assists_config.allowed = params
        .code_action_params
        .context
        .only
        .map(|it| it.into_iter().filter_map(from_proto::assist_kind).collect());

    let (assist_index, assist_resolve) = match parse_action_id(&params.id) {
        Ok(parsed_data) => parsed_data,
        Err(e) => {
            return Err(invalid_params_error(format!(
                "Failed to parse action id string '{}': {e}",
                params.id
            ))
            .into());
        }
    };

    let expected_assist_id = assist_resolve.assist_id.clone();
    let expected_kind = assist_resolve.assist_kind;

    let assists = snap.analysis.assists_with_fixes(
        &assists_config,
        &snap.config.diagnostic_fixes(Some(source_root)),
        AssistResolveStrategy::Single(assist_resolve),
        frange,
    )?;

    let assist = match assists.get(assist_index) {
        Some(assist) => assist,
        None => return Err(invalid_params_error(format!(
            "Failed to find the assist for index {} provided by the resolve request. Resolve request assist id: {}",
            assist_index, params.id,
        ))
        .into())
    };
    if assist.id.0 != expected_assist_id || assist.id.1 != expected_kind {
        return Err(invalid_params_error(format!(
            "Mismatching assist at index {} for the resolve parameters given. Resolve request assist id: {}, actual id: {:?}.",
            assist_index, params.id, assist.id
        ))
        .into());
    }
    let ca = to_proto::code_action(&snap, &snap.config.client_commands(), assist.clone(), None)?;
    code_action.edit = ca.edit;
    code_action.command = ca.command;

    if let Some(edit) = code_action.edit.as_ref()
        && let Some(changes) = edit.document_changes.as_ref()
    {
        for change in changes {
            if let lsp_ext::SnippetDocumentChangeOperation::Change(change) = change {
                resource_ops_supported(&snap.config, change)?
            }
        }
    }

    Ok(code_action)
}

fn parse_action_id(action_id: &str) -> anyhow::Result<(usize, SingleResolve), String> {
    let id_parts = action_id.split(':').collect::<Vec<_>>();
    match id_parts.as_slice() {
        [assist_id_string, assist_kind_string, index_string, subtype_str] => {
            let assist_kind: AssistKind = assist_kind_string.parse()?;
            let index: usize = match index_string.parse() {
                Ok(index) => index,
                Err(e) => return Err(format!("Incorrect index string: {e}")),
            };
            let assist_subtype = subtype_str.parse::<usize>().ok();
            Ok((
                index,
                SingleResolve {
                    assist_id: assist_id_string.to_string(),
                    assist_kind,
                    assist_subtype,
                },
            ))
        }
        _ => Err("Action id contains incorrect number of segments".to_owned()),
    }
}

pub(crate) fn handle_code_lens(
    snap: GlobalStateSnapshot,
    params: lsp_types::CodeLensParams,
) -> anyhow::Result<Option<Vec<CodeLens>>> {
    let _p = tracing::info_span!("handle_code_lens").entered();

    let lens_config = snap.config.lens();
    if lens_config.none() {
        // early return before any db query!
        return Ok(Some(Vec::default()));
    }

    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let target_spec = TargetSpec::for_file(&snap, file_id)?;

    let annotations = snap.analysis.annotations(
        &lens_config.into_annotation_config(
            target_spec
                .map(|spec| {
                    matches!(
                        spec.target_kind(),
                        TargetKind::Bin
                            | TargetKind::Example
                            | TargetKind::Test
                            | TargetKind::Bench
                    )
                })
                .unwrap_or(false),
            snap.minicore(),
        ),
        file_id,
    )?;

    let mut res = Vec::new();
    for a in annotations {
        to_proto::code_lens(&mut res, &snap, a)?;
    }

    Ok(Some(res))
}

pub(crate) fn handle_code_lens_resolve(
    snap: GlobalStateSnapshot,
    mut code_lens: CodeLens,
) -> anyhow::Result<CodeLens> {
    let Some(data) = code_lens.data.take() else {
        return Ok(code_lens);
    };
    let resolve = serde_json::from_value::<lsp_ext::CodeLensResolveData>(data)?;
    let Some(annotation) = from_proto::annotation(&snap, code_lens.range, resolve)? else {
        return Ok(code_lens);
    };
    let config = snap.config.lens().into_annotation_config(false, snap.minicore());
    let annotation = snap.analysis.resolve_annotation(&config, annotation)?;

    let mut acc = Vec::new();
    to_proto::code_lens(&mut acc, &snap, annotation)?;

    let mut res = match acc.pop() {
        Some(it) if acc.is_empty() => it,
        _ => {
            never!();
            code_lens
        }
    };
    res.data = None;

    Ok(res)
}

pub(crate) fn handle_document_highlight(
    snap: GlobalStateSnapshot,
    params: lsp_types::DocumentHighlightParams,
) -> anyhow::Result<Option<Vec<lsp_types::DocumentHighlight>>> {
    let _p = tracing::info_span!("handle_document_highlight").entered();
    let position =
        try_default!(from_proto::file_position(&snap, &params.text_document_position_params)?);
    let line_index = snap.file_line_index(position.file_id)?;
    let source_root = snap.analysis.source_root_id(position.file_id)?;

    let refs = match snap
        .analysis
        .highlight_related(snap.config.highlight_related(Some(source_root)), position)?
    {
        None => return Ok(None),
        Some(refs) => refs,
    };
    let res = refs
        .into_iter()
        .map(|ide::HighlightedRange { range, category }| lsp_types::DocumentHighlight {
            range: to_proto::range(&line_index, range),
            kind: to_proto::document_highlight_kind(category),
        })
        .collect();
    Ok(Some(res))
}

pub(crate) fn handle_ssr(
    snap: GlobalStateSnapshot,
    params: lsp_ext::SsrParams,
) -> anyhow::Result<lsp_types::WorkspaceEdit> {
    let _p = tracing::info_span!("handle_ssr").entered();
    let selections = try_default!(
        params
            .selections
            .iter()
            .map(|range| from_proto::file_range(&snap, &params.position.text_document, *range))
            .collect::<Result<Option<Vec<_>>, _>>()?
    );
    let position = try_default!(from_proto::file_position(&snap, &params.position)?);
    let source_change = snap.analysis.structural_search_replace(
        &params.query,
        params.parse_only,
        position,
        selections,
    )??;
    to_proto::workspace_edit(&snap, source_change).map_err(Into::into)
}

pub(crate) fn handle_inlay_hints(
    snap: GlobalStateSnapshot,
    params: InlayHintParams,
) -> anyhow::Result<Option<Vec<InlayHint>>> {
    let _p = tracing::info_span!("handle_inlay_hints").entered();
    let document_uri = &params.text_document.uri;
    let requested_lsp_range = params.range;
    let FileRange { file_id, range } = try_default!(from_proto::file_range(
        &snap,
        &TextDocumentIdentifier::new(document_uri.to_owned()),
        params.range,
    )?);
    let line_index = snap.file_line_index(file_id)?;
    let range = TextRange::new(
        range.start().min(line_index.index.len()),
        range.end().min(line_index.index.len()),
    );

    let inlay_hints_config = snap.config.inlay_hints(snap.minicore());
    let mut hints = snap
        .analysis
        .inlay_hints(&inlay_hints_config, file_id, Some(range))?
        .into_iter()
        .map(|it| {
            to_proto::inlay_hint(
                &snap,
                &inlay_hints_config.fields_to_resolve,
                &line_index,
                file_id,
                it,
            )
        })
        .collect::<Cancellable<Vec<_>>>()?;

    let events = ownership_events_for_file(&snap.ownership_events, file_id);
    let exact_positions: FxHashSet<_> = events
        .iter()
        .filter(|event| lsp_ranges_overlap(event.range, requested_lsp_range))
        .map(|event| event.range.end)
        .collect();
    hints.retain(|hint| {
        !exact_positions.contains(&hint.position)
            || !match &hint.label {
                lsp_types::Label::String(label) => label.contains('?'),
                lsp_types::Label::InlayHintLabelPartList(parts) => {
                    parts.iter().any(|part| part.value.contains('?'))
                }
            }
    });

    let mut grouped: FxHashMap<Position, Vec<_>> = FxHashMap::default();
    for event in events {
        if lsp_ranges_overlap(event.range, requested_lsp_range) {
            grouped.entry(event.range.end).or_default().push(event);
        }
    }
    for (position, events) in grouped {
        let labels = events.iter().map(|event| event.kind.label()).unique().join(" · ");
        let tooltip = events
            .iter()
            .map(|event| {
                let detail = event
                    .detail
                    .as_deref()
                    .map(|detail| format!(" — {detail}"))
                    .unwrap_or_default();
                format!("- `{}`: **{}**{detail}", event.name, event.kind.label())
            })
            .join("\n");
        hints.push(InlayHint {
            position,
            label: lsp_types::Label::String(labels),
            kind: None,
            text_edits: None,
            tooltip: Some(lsp_types::Tooltip::MarkupContent(lsp_types::MarkupContent {
                kind: lsp_types::MarkupKind::Markdown,
                value: format!("**Compiler ownership flow (MIR)**\n\n{tooltip}"),
            })),
            padding_left: Some(true),
            padding_right: Some(false),
            data: Some(json!({
                "rustWorkbench": {
                    "version": 1,
                    "category": "ownership",
                    "precision": "compiler_exact",
                    "eventKinds": events
                        .iter()
                        .map(|event| ownership_kind_code(event.kind))
                        .unique()
                        .collect::<Vec<_>>(),
                }
            })),
        });
    }
    if snap.config.ownership_mechanics_enabled(None) {
        let tutorial = crate::diagnostics::ownership_tutorial_for_file(
            &snap.ownership_tutorial_models,
            file_id,
        );
        hints.extend(ownership_mechanics_hints(
            &tutorial,
            requested_lsp_range,
            OwnershipMechanicsCategories {
                layout: snap.config.ownership_mechanics_category_enabled(None, "layout"),
                storage: snap.config.ownership_mechanics_category_enabled(None, "storage"),
                access: snap.config.ownership_mechanics_category_enabled(None, "access"),
                wrapper: snap.config.ownership_mechanics_category_enabled(None, "wrapper"),
            },
        ));
    }
    hints.sort_by_key(|hint| hint.position);
    Ok(Some(hints))
}

#[derive(Clone, Copy)]
struct OwnershipMechanicsCategories {
    layout: bool,
    storage: bool,
    access: bool,
    wrapper: bool,
}

fn ownership_mechanics_hints(
    tutorial: &crate::diagnostics::OwnershipTutorialModel,
    requested_range: Range,
    categories: OwnershipMechanicsCategories,
) -> Vec<InlayHint> {
    const MAX_MECHANICS_HINTS: usize = 128;
    #[derive(Debug)]
    struct MechanicsPart {
        category: &'static str,
        label: String,
        tooltip: String,
        binding_id: Option<String>,
        graph_node_id: Option<String>,
    }

    let mut grouped: Vec<(Position, Vec<MechanicsPart>)> = Vec::new();
    let target_triple = if tutorial.target_triple.is_empty() {
        "unknown target".to_owned()
    } else {
        tutorial.target_triple.clone()
    };
    let mut push = |position: Position,
                    category: &'static str,
                    label: String,
                    tooltip: String,
                    binding_id: Option<String>,
                    graph_node_id: Option<String>| {
        let part = MechanicsPart { category, label, tooltip, binding_id, graph_node_id };
        if let Some((_, parts)) = grouped.iter_mut().find(|(anchor, _)| *anchor == position) {
            if !parts
                .iter()
                .any(|existing| existing.category == part.category && existing.label == part.label)
            {
                parts.push(part);
            }
        } else if grouped.len() < MAX_MECHANICS_HINTS {
            grouped.push((position, vec![part]));
        }
    };

    for binding in tutorial
        .bindings
        .iter()
        .filter(|binding| lsp_ranges_overlap(binding.range, requested_range))
    {
        let binding_id = format!("{:016x}-{}", binding.body_id, binding.name);
        if categories.layout
            && let (Some(size), Some(align)) = (binding.size, binding.align)
        {
            let inline = binding.memory_layers.iter().all(|layer| layer.storage != "heap");
            push(
                binding.range.end,
                "layout",
                format!("{size} B · align {align} · {}", if inline { "inline" } else { "handle" }),
                format!(
                    "**Layout of `{}`**\n\n- Type: `{}`\n- Handle/value size: **{size} bytes**\n- Alignment: **{align} bytes**\n- Target: `{target_triple}`\n- Precision: **compiler exact target layout**\n\nRuntime allocation sizes are kept separate.",
                    binding.name, binding.type_name,
                ),
                Some(binding_id.clone()),
                None,
            );
        }
        let heap_layers = binding
            .memory_layers
            .iter()
            .filter(|layer| layer.storage == "heap")
            .map(|layer| layer.label.as_str())
            .unique()
            .take(2)
            .join(" + ");
        if categories.storage && !heap_layers.is_empty() {
            push(
                binding.range.end,
                "storage",
                format!("handle → {heap_layers}"),
                format!(
                    "**Storage reached from `{}`**\n\n`{}` is the local handle. It reaches **{heap_layers}**. Moving the handle does not copy the allocation. Runtime addresses, lengths, capacities, and reference counts are unknown.",
                    binding.name, binding.type_name
                ),
                Some(binding_id.clone()),
                None,
            );
        }
        let wrappers = binding
            .memory_layers
            .iter()
            .filter(|layer| layer.kind != "stack_binding")
            .map(|layer| layer.kind.replace('_', " "))
            .unique()
            .take(3)
            .join(" → ");
        if categories.wrapper && !wrappers.is_empty() {
            push(
                binding.range.end,
                "wrapper",
                wrappers.clone(),
                format!(
                    "**Wrapper route for `{}`**\n\n`{}`\n\n{wrappers}\n\nThis is a source-level ownership model, not a sampled runtime memory address or counter.",
                    binding.name, binding.type_name
                ),
                Some(binding_id),
                None,
            );
        }
    }

    if categories.access {
        for path in tutorial.memory_graph.access_paths.iter().take(MAX_MECHANICS_HINTS) {
            let Some(node) =
                tutorial.memory_graph.nodes.iter().find(|node| node.node.id == path.node_id)
            else {
                continue;
            };
            let Some(range) = node.range else { continue };
            if !lsp_ranges_overlap(range, requested_range) || path.steps.is_empty() {
                continue;
            }
            let label = path
                .steps
                .iter()
                .map(|step| format!("{} → {}", step.kind.replace('_', " "), step.result_type))
                .take(2)
                .join(" · ");
            let details = path
                .steps
                .iter()
                .map(|step| {
                    let mut constraints = Vec::new();
                    if step.fallible {
                        constraints.push("fallible");
                    }
                    if step.may_panic {
                        constraints.push("may panic");
                    }
                    if step.requires_unsafe {
                        constraints.push("requires unsafe");
                    }
                    format!(
                        "- `{}` → `{}` via **{}** ({}, {}{}) — {}",
                        step.starting_type,
                        step.result_type,
                        step.kind.replace('_', " "),
                        step.mutability.replace('_', " "),
                        step.explicitness.replace('_', " "),
                        if constraints.is_empty() {
                            String::new()
                        } else {
                            format!(", {}", constraints.join(", "))
                        },
                        step.explanation
                    )
                })
                .join("\n");
            push(
                range.end,
                "access",
                label,
                format!("**How `{}` becomes usable**\n\n{details}", path.place),
                None,
                Some(path.node_id.clone()),
            );
        }
    }
    grouped
        .into_iter()
        .map(|(position, parts)| {
            let label = parts.iter().map(|part| part.label.as_str()).join(" · ");
            let tooltip = parts.iter().map(|part| part.tooltip.as_str()).join("\n\n---\n\n");
            let categories = parts.iter().map(|part| part.category).collect::<Vec<_>>();
            let segments = parts
                .iter()
                .map(|part| json!({ "category": part.category, "label": part.label }))
                .collect::<Vec<_>>();
            let binding_id = parts.iter().find_map(|part| part.binding_id.as_deref());
            let graph_node_id = parts.iter().find_map(|part| part.graph_node_id.as_deref());
            InlayHint {
                position,
                label: lsp_types::Label::String(label),
                kind: None,
                text_edits: None,
                // Keep detailed mechanics prose out of the hot visible-range
                // response. Clients request it through inlayHint/resolve only
                // when the learner actually hovers the merged clue.
                tooltip: None,
                padding_left: Some(true),
                padding_right: Some(false),
                data: Some(json!({
                    "rustWorkbench": {
                        "version": 3,
                        "category": "mechanics",
                        "categories": categories,
                        "segments": segments,
                        "precision": "compiler_exact",
                        "problemId": null,
                        "bindingId": binding_id,
                        "graphNodeId": graph_node_id,
                        "focusRange": {
                            "start": position,
                            "end": position,
                        },
                    },
                    "rustWorkbenchTooltip": tooltip,
                })),
            }
        })
        .collect()
}

fn lsp_ranges_overlap(left: Range, right: Range) -> bool {
    left.start <= right.end && right.start <= left.end
}

pub(crate) fn handle_inlay_hints_resolve(
    snap: GlobalStateSnapshot,
    mut original_hint: InlayHint,
) -> anyhow::Result<InlayHint> {
    let _p = tracing::info_span!("handle_inlay_hints_resolve").entered();

    let Some(data) = original_hint.data.take() else {
        return Ok(original_hint);
    };
    if let Some(tooltip) = data.get("rustWorkbenchTooltip").and_then(|value| value.as_str()) {
        original_hint.tooltip = Some(lsp_types::Tooltip::MarkupContent(lsp_types::MarkupContent {
            kind: lsp_types::MarkupKind::Markdown,
            value: tooltip.to_owned(),
        }));
        original_hint.data = Some(data);
        return Ok(original_hint);
    }
    if rust_workbench_metadata_only_inlay_data(&data) {
        original_hint.data = Some(data);
        return Ok(original_hint);
    }
    let resolve_data: lsp_ext::InlayHintResolveData = serde_json::from_value(data)?;
    let file_id = FileId::from_raw(resolve_data.file_id);
    if resolve_data.version != snap.file_version(file_id) {
        tracing::warn!("Inlay hint resolve data is outdated");
        return Ok(original_hint);
    }
    let Some(hash) = resolve_data.hash.parse().ok() else {
        return Ok(original_hint);
    };
    anyhow::ensure!(snap.file_exists(file_id), "Invalid LSP resolve data");

    let line_index = snap.file_line_index(file_id)?;
    let range = from_proto::text_range(&line_index, resolve_data.resolve_range)?;

    let mut forced_resolve_inlay_hints_config = snap.config.inlay_hints(snap.minicore());
    forced_resolve_inlay_hints_config.fields_to_resolve = InlayFieldsToResolve::empty();
    let resolve_hints = snap.analysis.inlay_hints_resolve(
        &forced_resolve_inlay_hints_config,
        file_id,
        range,
        hash,
        |hint| {
            std::hash::BuildHasher::hash_one(
                &std::hash::BuildHasherDefault::<ide_db::FxHasher>::default(),
                hint,
            )
        },
    )?;

    Ok(resolve_hints
        .and_then(|it| {
            to_proto::inlay_hint(
                &snap,
                &forced_resolve_inlay_hints_config.fields_to_resolve,
                &line_index,
                file_id,
                it,
            )
            .ok()
        })
        .filter(|hint| hint.position == original_hint.position)
        .filter(|hint| hint.kind == original_hint.kind)
        .unwrap_or(original_hint))
}

fn rust_workbench_metadata_only_inlay_data(data: &serde_json::Value) -> bool {
    data.get("rustWorkbench").is_some()
        && data.get("file_id").is_none()
        && data.get("hash").is_none()
        && data.get("resolve_range").is_none()
}

pub(crate) fn handle_call_hierarchy_prepare(
    snap: GlobalStateSnapshot,
    params: CallHierarchyPrepareParams,
) -> anyhow::Result<Option<Vec<CallHierarchyItem>>> {
    let _p = tracing::info_span!("handle_call_hierarchy_prepare").entered();
    let position =
        try_default!(from_proto::file_position(&snap, &params.text_document_position_params)?);

    let config = snap.config.call_hierarchy(snap.minicore());
    let nav_info = match snap.analysis.call_hierarchy(position, &config)? {
        None => return Ok(None),
        Some(it) => it,
    };

    let RangeInfo { range: _, info: navs } = nav_info;
    let res = navs
        .into_iter()
        .filter(|it| matches!(it.kind, Some(SymbolKind::Function | SymbolKind::Method)))
        .map(|it| to_proto::call_hierarchy_item(&snap, it))
        .collect::<Cancellable<Vec<_>>>()?;

    Ok(Some(res))
}

pub(crate) fn handle_call_hierarchy_incoming(
    snap: GlobalStateSnapshot,
    params: CallHierarchyIncomingCallsParams,
) -> anyhow::Result<Option<Vec<CallHierarchyIncomingCall>>> {
    let _p = tracing::info_span!("handle_call_hierarchy_incoming").entered();
    let item = params.item;

    let doc = TextDocumentIdentifier::new(item.uri);
    let frange = try_default!(from_proto::file_range(&snap, &doc, item.selection_range)?);
    let fpos = FilePosition { file_id: frange.file_id, offset: frange.range.start() };

    let config = snap.config.call_hierarchy(snap.minicore());
    let call_items = match snap.analysis.incoming_calls(&config, fpos)? {
        None => return Ok(None),
        Some(it) => it,
    };

    Ok(Some(
        call_items
            .into_iter()
            .map(|call_item| {
                let file_id = call_item.target.file_id;
                let line_index = snap.file_line_index(file_id)?;
                let item = to_proto::call_hierarchy_item(&snap, call_item.target)?;
                Ok(CallHierarchyIncomingCall {
                    from: item,
                    from_ranges: call_item
                        .ranges
                        .iter()
                        // This is the range relative to the item
                        .filter(|it| it.file_id == file_id)
                        .map(|it| to_proto::range(&line_index, it.range))
                        .collect(),
                })
            })
            .collect::<anyhow::Result<_>>()?,
    ))
}

pub(crate) fn handle_call_hierarchy_outgoing(
    snap: GlobalStateSnapshot,
    params: CallHierarchyOutgoingCallsParams,
) -> anyhow::Result<Option<Vec<CallHierarchyOutgoingCall>>> {
    let _p = tracing::info_span!("handle_call_hierarchy_outgoing").entered();
    let item = params.item;

    let doc = TextDocumentIdentifier::new(item.uri);
    let frange = try_default!(from_proto::file_range(&snap, &doc, item.selection_range)?);
    let fpos = FilePosition { file_id: frange.file_id, offset: frange.range.start() };
    let line_index = snap.file_line_index(fpos.file_id)?;

    let config = snap.config.call_hierarchy(snap.minicore());
    let call_items = match snap.analysis.outgoing_calls(&config, fpos)? {
        None => return Ok(None),
        Some(it) => it,
    };

    let mut res = vec![];

    for call_item in call_items.into_iter() {
        let item = to_proto::call_hierarchy_item(&snap, call_item.target)?;
        res.push(CallHierarchyOutgoingCall {
            to: item,
            from_ranges: call_item
                .ranges
                .into_iter()
                // This is the range relative to the caller
                .filter(|it| it.file_id == fpos.file_id)
                .map(|it| to_proto::range(&line_index, it.range))
                .collect(),
        });
    }

    Ok(Some(res))
}

pub(crate) fn handle_semantic_tokens_full(
    snap: GlobalStateSnapshot,
    params: SemanticTokensParams,
) -> anyhow::Result<Option<SemanticTokens>> {
    let _p = tracing::info_span!("handle_semantic_tokens_full").entered();

    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let text = snap.analysis.file_text(file_id)?;
    let line_index = snap.file_line_index(file_id)?;

    let mut highlight_config = snap.config.highlighting_config(snap.minicore());
    // Avoid flashing a bunch of unresolved references when the proc-macro servers haven't been spawned yet.
    highlight_config.syntactic_name_ref_highlighting =
        snap.workspaces.is_empty() || !snap.proc_macros_loaded;

    let highlights = snap.analysis.highlight(highlight_config, file_id)?;
    let mut semantic_tokens = to_proto::semantic_tokens(
        &text,
        &line_index,
        highlights,
        snap.config.semantics_tokens_augments_syntax_tokens(),
        snap.config.highlighting_non_standard_tokens(),
    );
    enrich_ownership_tokens(
        &mut semantic_tokens,
        &ownership_events_for_file(&snap.ownership_events, file_id),
    );

    // Unconditionally cache the tokens
    snap.semantic_tokens_cache.lock().insert(params.text_document.uri, semantic_tokens.clone());

    Ok(Some(semantic_tokens))
}

pub(crate) fn handle_semantic_tokens_full_delta(
    snap: GlobalStateSnapshot,
    params: SemanticTokensDeltaParams,
) -> anyhow::Result<Option<SemanticTokensDeltaResponse>> {
    let _p = tracing::info_span!("handle_semantic_tokens_full_delta").entered();

    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let text = snap.analysis.file_text(file_id)?;
    let line_index = snap.file_line_index(file_id)?;

    let mut highlight_config = snap.config.highlighting_config(snap.minicore());
    // Avoid flashing a bunch of unresolved references when the proc-macro servers haven't been spawned yet.
    highlight_config.syntactic_name_ref_highlighting =
        snap.workspaces.is_empty() || !snap.proc_macros_loaded;

    let highlights = snap.analysis.highlight(highlight_config, file_id)?;
    let mut semantic_tokens = to_proto::semantic_tokens(
        &text,
        &line_index,
        highlights,
        snap.config.semantics_tokens_augments_syntax_tokens(),
        snap.config.highlighting_non_standard_tokens(),
    );
    enrich_ownership_tokens(
        &mut semantic_tokens,
        &ownership_events_for_file(&snap.ownership_events, file_id),
    );

    let cached_tokens = snap.semantic_tokens_cache.lock().remove(&params.text_document.uri);

    if let Some(cached_tokens @ lsp_types::SemanticTokens { result_id: Some(prev_id), .. }) =
        &cached_tokens
        && *prev_id == params.previous_result_id
    {
        let delta = to_proto::semantic_token_delta(cached_tokens, &semantic_tokens);
        snap.semantic_tokens_cache.lock().insert(params.text_document.uri, semantic_tokens);
        return Ok(Some(delta.into()));
    }

    // Clone first to keep the lock short
    let semantic_tokens_clone = semantic_tokens.clone();
    snap.semantic_tokens_cache.lock().insert(params.text_document.uri, semantic_tokens_clone);

    Ok(Some(semantic_tokens.into()))
}

pub(crate) fn handle_semantic_tokens_range(
    snap: GlobalStateSnapshot,
    params: SemanticTokensRangeParams,
) -> anyhow::Result<Option<SemanticTokens>> {
    let _p = tracing::info_span!("handle_semantic_tokens_range").entered();

    let frange = try_default!(from_proto::file_range(&snap, &params.text_document, params.range)?);
    let text = snap.analysis.file_text(frange.file_id)?;
    let line_index = snap.file_line_index(frange.file_id)?;

    let mut highlight_config = snap.config.highlighting_config(snap.minicore());
    // Avoid flashing a bunch of unresolved references when the proc-macro servers haven't been spawned yet.
    highlight_config.syntactic_name_ref_highlighting =
        snap.workspaces.is_empty() || !snap.proc_macros_loaded;

    let highlights = snap.analysis.highlight_range(highlight_config, frange)?;
    let mut semantic_tokens = to_proto::semantic_tokens(
        &text,
        &line_index,
        highlights,
        snap.config.semantics_tokens_augments_syntax_tokens(),
        snap.config.highlighting_non_standard_tokens(),
    );
    enrich_ownership_tokens(
        &mut semantic_tokens,
        &ownership_events_for_file(&snap.ownership_events, frange.file_id),
    );
    Ok(Some(semantic_tokens))
}

fn enrich_ownership_tokens(
    semantic_tokens: &mut SemanticTokens,
    events: &[crate::diagnostics::OwnershipEvent],
) {
    let mut line = 0;
    let mut character = 0;
    for token in &mut semantic_tokens.data {
        line += token.delta_line;
        if token.delta_line == 0 {
            character += token.delta_start;
        } else {
            character = token.delta_start;
        }
        let token_range = Range::new(
            Position::new(line, character),
            Position::new(line, character + token.length),
        );
        for event in events.iter().filter(|event| lsp_ranges_overlap(event.range, token_range)) {
            if let Some(bit) = crate::lsp::semantic_tokens::modifier_bit(event.kind.modifier()) {
                token.token_modifiers_bitset |= bit;
            }
        }
    }
}

pub(crate) fn handle_open_docs(
    snap: GlobalStateSnapshot,
    params: lsp_types::TextDocumentPositionParams,
) -> anyhow::Result<ExternalDocsResponse> {
    let _p = tracing::info_span!("handle_open_docs").entered();
    let position = try_default!(from_proto::file_position(&snap, &params)?);

    let ws_and_sysroot = snap.workspaces.iter().find_map(|ws| match &ws.kind {
        ProjectWorkspaceKind::Cargo { cargo, .. }
        | ProjectWorkspaceKind::DetachedFile { cargo: Some((cargo, _, _)), .. } => {
            Some((cargo, &ws.sysroot))
        }
        ProjectWorkspaceKind::Json { .. } => None,
        ProjectWorkspaceKind::DetachedFile { .. } => None,
    });

    let (cargo, sysroot) = match ws_and_sysroot {
        Some((ws, sysroot)) => (Some(ws), Some(sysroot)),
        _ => (None, None),
    };

    let sysroot = sysroot.and_then(|p| p.root()).map(|it| it.as_str());
    let target_dir = cargo.map(|cargo| cargo.target_directory()).map(|p| p.as_str());

    let Ok(remote_urls) = snap.analysis.external_docs(position, target_dir, sysroot) else {
        return if snap.config.local_docs() {
            Ok(ExternalDocsResponse::WithLocal(Default::default()))
        } else {
            Ok(ExternalDocsResponse::Simple(None))
        };
    };

    let web = remote_urls.web_url.and_then(|it| Uri::parse(&it).ok());
    let local = remote_urls.local_url.and_then(|it| Uri::parse(&it).ok());

    if snap.config.local_docs() {
        Ok(ExternalDocsResponse::WithLocal(ExternalDocsPair { web, local }))
    } else {
        Ok(ExternalDocsResponse::Simple(web))
    }
}

pub(crate) fn handle_open_cargo_toml(
    snap: GlobalStateSnapshot,
    params: lsp_ext::OpenCargoTomlParams,
) -> anyhow::Result<Option<lsp_types::DefinitionResponse>> {
    let _p = tracing::info_span!("handle_open_cargo_toml").entered();
    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);

    let cargo_spec = match TargetSpec::for_file(&snap, file_id)? {
        Some(TargetSpec::Cargo(it)) => it,
        Some(TargetSpec::ProjectJson(_)) | None => return Ok(None),
    };

    let cargo_toml_url = to_proto::url_from_abs_path(&cargo_spec.cargo_toml);
    let res = lsp_types::DefinitionResponse::Definition(lsp_types::Definition::Location(
        Location::new(cargo_toml_url, Range::default()),
    ));
    Ok(Some(res))
}

pub(crate) fn handle_move_item(
    snap: GlobalStateSnapshot,
    params: lsp_ext::MoveItemParams,
) -> anyhow::Result<Vec<lsp_ext::SnippetTextEdit>> {
    let _p = tracing::info_span!("handle_move_item").entered();
    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let range = try_default!(from_proto::file_range(&snap, &params.text_document, params.range)?);

    let direction = match params.direction {
        lsp_ext::MoveItemDirection::Up => ide::Direction::Up,
        lsp_ext::MoveItemDirection::Down => ide::Direction::Down,
    };

    match snap.analysis.move_item(range, direction)? {
        Some(text_edit) => {
            let line_index = snap.file_line_index(file_id)?;
            Ok(to_proto::snippet_text_edit_vec(
                &line_index,
                true,
                text_edit,
                snap.config.change_annotation_support(),
            ))
        }
        None => Ok(vec![]),
    }
}

pub(crate) fn handle_view_recursive_memory_layout(
    snap: GlobalStateSnapshot,
    params: lsp_types::TextDocumentPositionParams,
) -> anyhow::Result<Option<lsp_ext::RecursiveMemoryLayout>> {
    let _p = tracing::info_span!("handle_view_recursive_memory_layout").entered();
    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let line_index = snap.file_line_index(file_id)?;
    let offset = from_proto::offset(&line_index, params.position)?;

    let res = snap.analysis.get_recursive_memory_layout(FilePosition { file_id, offset })?;
    Ok(res.map(|it| lsp_ext::RecursiveMemoryLayout {
        nodes: it
            .nodes
            .iter()
            .map(|n| lsp_ext::MemoryLayoutNode {
                item_name: n.item_name.clone(),
                typename: n.typename.clone(),
                size: n.size,
                offset: n.offset,
                alignment: n.alignment,
                parent_idx: n.parent_idx,
                children_start: n.children_start,
                children_len: n.children_len,
            })
            .collect(),
    }))
}

fn to_command_link(command: lsp_types::Command, tooltip: String) -> lsp_ext::CommandLink {
    lsp_ext::CommandLink { tooltip: Some(tooltip), command }
}

fn show_impl_command_link(
    snap: &GlobalStateSnapshot,
    position: &FilePosition,
    implementations: bool,
    show_references: bool,
) -> Option<lsp_ext::CommandLinkGroup> {
    if implementations
        && show_references
        && let Some(nav_data) = snap
            .analysis
            .goto_implementation(&snap.config.goto_implementation(), *position)
            .unwrap_or(None)
    {
        let uri = to_proto::url(snap, position.file_id);
        let line_index = snap.file_line_index(position.file_id).ok()?;
        let position = to_proto::position(&line_index, position.offset);
        let locations: Vec<_> = nav_data
            .info
            .into_iter()
            .filter_map(|nav| to_proto::location_from_nav(snap, &nav).ok())
            .collect();
        let title = to_proto::implementation_title(locations.len());
        let command = to_proto::command::show_references(title, &uri, position, locations);

        return Some(lsp_ext::CommandLinkGroup {
            commands: vec![to_command_link(command, "Go to implementations".into())],
            ..Default::default()
        });
    }
    None
}

fn show_ref_command_link(
    snap: &GlobalStateSnapshot,
    position: &FilePosition,
    references: bool,
    show_reference: bool,
) -> Option<lsp_ext::CommandLinkGroup> {
    if references
        && show_reference
        && let Some(ref_search_res) = snap
            .analysis
            .find_all_refs(
                *position,
                &FindAllRefsConfig {
                    search_scope: None,

                    ra_fixture: snap.config.ra_fixture(snap.minicore()),
                    exclude_imports: snap.config.find_all_refs_exclude_imports(),
                    exclude_tests: snap.config.find_all_refs_exclude_tests(),
                },
            )
            .unwrap_or(None)
    {
        let uri = to_proto::url(snap, position.file_id);
        let line_index = snap.file_line_index(position.file_id).ok()?;
        let position = to_proto::position(&line_index, position.offset);
        let locations: Vec<_> = ref_search_res
            .into_iter()
            .flat_map(|res| res.references)
            .flat_map(|(file_id, ranges)| {
                ranges.into_iter().map(move |(range, _)| FileRange { file_id, range })
            })
            .unique()
            .filter_map(|range| to_proto::location(snap, range).ok())
            .collect();
        let title = to_proto::reference_title(locations.len());
        let command = to_proto::command::show_references(title, &uri, position, locations);

        return Some(lsp_ext::CommandLinkGroup {
            commands: vec![to_command_link(command, "Go to references".into())],
            ..Default::default()
        });
    }
    None
}

fn runnable_action_links(
    snap: &GlobalStateSnapshot,
    runnable: Runnable,
    hover_actions_config: &HoverActionsConfig,
    client_commands_config: &ClientCommandsConfig,
) -> Option<lsp_ext::CommandLinkGroup> {
    if !hover_actions_config.runnable() {
        return None;
    }

    let target_spec = TargetSpec::for_file(snap, runnable.nav.file_id).ok()?;
    if should_skip_target(&runnable, target_spec.as_ref()) {
        return None;
    }

    if !(client_commands_config.run_single || client_commands_config.debug_single) {
        return None;
    }

    let title = runnable.title();
    let update_test = runnable.update_test;
    let r = to_proto::runnable(snap, runnable).ok()??;

    let mut group = lsp_ext::CommandLinkGroup::default();

    if hover_actions_config.run && client_commands_config.run_single {
        let run_command = to_proto::command::run_single(&r, &title);
        group.commands.push(to_command_link(run_command, r.label.clone()));
    }

    if hover_actions_config.debug && client_commands_config.debug_single {
        let dbg_command = to_proto::command::debug_single(&r);
        group.commands.push(to_command_link(dbg_command, r.label.clone()));
    }

    if hover_actions_config.update_test && client_commands_config.run_single {
        let label = update_test.label();
        if let Some(r) = to_proto::make_update_runnable(&r, update_test) {
            let update_command = to_proto::command::run_single(&r, label.unwrap().as_str());
            group.commands.push(to_command_link(update_command, r.label));
        }
    }

    Some(group)
}

fn goto_type_action_links(
    snap: &GlobalStateSnapshot,
    nav_targets: &[HoverGotoTypeData],
    hover_actions: &HoverActionsConfig,
    client_commands: &ClientCommandsConfig,
) -> Option<lsp_ext::CommandLinkGroup> {
    if !hover_actions.goto_type_def || nav_targets.is_empty() || !client_commands.goto_location {
        return None;
    }

    Some(lsp_ext::CommandLinkGroup {
        title: Some("Go to ".into()),
        commands: nav_targets
            .iter()
            .filter_map(|it| {
                to_proto::command::goto_location(snap, &it.nav)
                    .map(|cmd| to_command_link(cmd, it.mod_path.clone()))
            })
            .collect(),
    })
}

fn prepare_hover_actions(
    snap: &GlobalStateSnapshot,
    actions: &[HoverAction],
) -> Vec<lsp_ext::CommandLinkGroup> {
    let hover_actions = snap.config.hover_actions();
    let client_commands = snap.config.client_commands();
    actions
        .iter()
        .filter_map(|it| match it {
            HoverAction::Implementation(position) => show_impl_command_link(
                snap,
                position,
                hover_actions.implementations,
                client_commands.show_reference,
            ),
            HoverAction::Reference(position) => show_ref_command_link(
                snap,
                position,
                hover_actions.references,
                client_commands.show_reference,
            ),
            HoverAction::Runnable(r) => {
                runnable_action_links(snap, r.clone(), &hover_actions, &client_commands)
            }
            HoverAction::GoToType(targets) => {
                goto_type_action_links(snap, targets, &hover_actions, &client_commands)
            }
        })
        .collect()
}

fn should_skip_target(runnable: &Runnable, cargo_spec: Option<&TargetSpec>) -> bool {
    match runnable.kind {
        RunnableKind::Bin => {
            // Do not suggest binary run on other target than binary
            match &cargo_spec {
                Some(spec) => !matches!(
                    spec.target_kind(),
                    TargetKind::Bin | TargetKind::Example | TargetKind::Test | TargetKind::Bench
                ),
                None => true,
            }
        }
        _ => false,
    }
}

fn run_rustfmt(
    snap: &GlobalStateSnapshot,
    text_document: &TextDocumentIdentifier,
    range: Option<lsp_types::Range>,
) -> anyhow::Result<Option<Vec<lsp_types::TextEdit>>> {
    let file_id = try_default!(from_proto::file_id(snap, &text_document.uri)?);
    let file = snap.analysis.file_text(file_id)?;

    let line_index = snap.file_line_index(file_id)?;
    let source_root_id = snap.analysis.source_root_id(file_id).ok();
    let crates = snap.analysis.relevant_crates_for(file_id)?;

    // try to chdir to the file so we can respect `rustfmt.toml`
    // FIXME: use `rustfmt --config-path` once
    // https://github.com/rust-lang/rustfmt/issues/4660 gets fixed
    let current_dir = match text_document.uri.to_file_path() {
        Ok(mut path) => {
            // pop off file name
            if path.pop() && path.is_dir() { path } else { std::env::current_dir()? }
        }
        Err(_) => {
            tracing::error!(
                text_document = ?text_document.uri,
                "Unable to get path, rustfmt.toml might be ignored"
            );
            std::env::current_dir()?
        }
    };

    let mut command = match snap.config.rustfmt(source_root_id) {
        RustfmtConfig::Rustfmt { extra_args, enable_range_formatting } => {
            // Determine the edition of the crate the file belongs to (if there's multiple, we pick the
            // highest edition).
            let Ok(editions) = crates
                .iter()
                .map(|&crate_id| snap.analysis.crate_edition(crate_id))
                .collect::<Result<Vec<_>, _>>()
            else {
                return Ok(None);
            };
            let edition = editions.iter().copied().max();

            // FIXME: Set RUSTUP_TOOLCHAIN
            let mut cmd = toolchain::command(
                toolchain::Tool::Rustfmt.path(),
                current_dir,
                snap.config.extra_env(source_root_id),
            );
            cmd.args(extra_args);

            if let Some(edition) = edition {
                cmd.arg("--edition");
                cmd.arg(edition.to_string());
            }

            if let Some(range) = range {
                if !enable_range_formatting {
                    return Err(LspError::new(
                        ErrorCode::InvalidRequest as i32,
                        String::from(
                            "rustfmt range formatting is unstable. \
                            Opt-in by using a nightly build of rustfmt and setting \
                            `rustfmt.rangeFormatting.enable` to true in your LSP configuration",
                        ),
                    )
                    .into());
                }

                let frange = try_default!(from_proto::file_range(snap, text_document, range)?);
                let start_line = line_index.index.line_col(frange.range.start()).line;
                let end_line = line_index.index.line_col(frange.range.end()).line;

                cmd.arg("--unstable-features");
                cmd.arg("--file-lines");
                cmd.arg(
                    json!([{
                        "file": "stdin",
                        // LineCol is 0-based, but rustfmt is 1-based.
                        "range": [start_line + 1, end_line + 1]
                    }])
                    .to_string(),
                );
            }

            cmd
        }
        RustfmtConfig::CustomCommand { command, args } => {
            let cmd = Utf8PathBuf::from(&command);
            let target_spec = TargetSpec::for_file(snap, file_id).ok().flatten();
            let extra_env = snap.config.extra_env(source_root_id);
            let mut cmd = match target_spec {
                Some(TargetSpec::Cargo(_)) => {
                    // approach: if the command name contains a path separator, join it with the project root.
                    // however, if the path is absolute, joining will result in the absolute path being preserved.
                    // as a fallback, rely on $PATH-based discovery.
                    let cmd_path = if command.contains(std::path::MAIN_SEPARATOR)
                        || (cfg!(windows) && command.contains('/'))
                    {
                        let project_root = Utf8PathBuf::from_path_buf(current_dir.clone())
                            .ok()
                            .and_then(|p| AbsPathBuf::try_from(p).ok());
                        let project_root = project_root
                            .as_ref()
                            .map(|dir| snap.config.workspace_root_for(dir))
                            .unwrap_or(snap.config.default_root_path());
                        project_root.join(cmd).into()
                    } else {
                        cmd
                    };
                    toolchain::command(cmd_path, current_dir, extra_env)
                }
                _ => toolchain::command(cmd, current_dir, extra_env),
            };

            cmd.args(args);
            cmd
        }
    };

    let output = {
        let _p = tracing::info_span!("rustfmt", ?command).entered();

        let mut rustfmt = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context(format!("Failed to spawn {command:?}"))?;

        rustfmt.stdin.as_mut().unwrap().write_all(file.as_bytes())?;

        rustfmt.wait_with_output()?
    };

    let captured_stdout = String::from_utf8(output.stdout)?;
    let captured_stderr = String::from_utf8(output.stderr).unwrap_or_default();

    if !output.status.success() {
        let rustfmt_not_installed =
            captured_stderr.contains("not installed") || captured_stderr.contains("not available");

        return match output.status.code() {
            Some(1) if !rustfmt_not_installed => {
                // While `rustfmt` doesn't have a specific exit code for parse errors this is the
                // likely cause exiting with 1. Most Language Servers swallow parse errors on
                // formatting because otherwise an error is surfaced to the user on top of the
                // syntax error diagnostics they're already receiving. This is especially jarring
                // if they have format on save enabled.
                tracing::warn!(
                    ?command,
                    %captured_stderr,
                    "rustfmt exited with status 1"
                );
                Ok(None)
            }
            // rustfmt panicked at lexing/parsing the file
            Some(101)
                if !rustfmt_not_installed
                    && (captured_stderr.starts_with("error[")
                        || captured_stderr.starts_with("error:")) =>
            {
                Ok(None)
            }
            _ => {
                // Something else happened - e.g. `rustfmt` is missing or caught a signal
                tracing::error!(
                    ?command,
                    %output.status,
                    %captured_stdout,
                    %captured_stderr,
                    "rustfmt failed"
                );
                Ok(None)
            }
        };
    }

    let (new_text, new_line_endings) = LineEndings::normalize(captured_stdout);

    if line_index.endings != new_line_endings {
        // If line endings are different, send the entire file.
        // Diffing would not work here, as the line endings might be the only
        // difference.
        Ok(Some(to_proto::text_edit_vec(
            &line_index,
            TextEdit::replace(TextRange::up_to(TextSize::of(&*file)), new_text),
        )))
    } else if *file == new_text {
        // The document is already formatted correctly -- no edits needed.
        Ok(None)
    } else {
        Ok(Some(to_proto::text_edit_vec(&line_index, diff(&file, &new_text))))
    }
}

pub(crate) fn fetch_dependency_list(
    state: GlobalStateSnapshot,
    _params: FetchDependencyListParams,
) -> anyhow::Result<FetchDependencyListResult> {
    let crates = state.analysis.fetch_crates()?;
    let crate_infos = crates
        .into_iter()
        .filter_map(|it| {
            let root_file_path = state.file_id_to_file_path(it.root_file_id);
            crate_path(&root_file_path).and_then(to_url).map(|path| CrateInfoResult {
                name: it.name,
                version: it.version,
                path,
            })
        })
        .collect();
    Ok(FetchDependencyListResult { crates: crate_infos })
}

pub(crate) fn internal_testing_fetch_config(
    state: GlobalStateSnapshot,
    params: InternalTestingFetchConfigParams,
) -> anyhow::Result<Option<InternalTestingFetchConfigResponse>> {
    let source_root = match params.text_document {
        Some(it) => Some(
            state
                .analysis
                .source_root_id(try_default!(from_proto::file_id(&state, &it.uri)?))
                .map_err(anyhow::Error::from)?,
        ),
        None => None,
    };
    Ok(Some(match params.config {
        InternalTestingFetchConfigOption::AssistEmitMustUse => {
            InternalTestingFetchConfigResponse::AssistEmitMustUse(
                state.config.assist(source_root).assist_emit_must_use,
            )
        }
        InternalTestingFetchConfigOption::CheckWorkspace => {
            InternalTestingFetchConfigResponse::CheckWorkspace(
                state.config.flycheck_workspace(source_root),
            )
        }
    }))
}

pub(crate) fn handle_evaluate_predicate(
    snap: GlobalStateSnapshot,
    params: lsp_ext::EvaluatePredicateParams,
) -> anyhow::Result<lsp_ext::EvaluatePredicateResult> {
    let _p = tracing::info_span!("handle_evaluate_predicate").entered();
    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let line_index = snap.file_line_index(file_id)?;
    let offset = from_proto::offset(&line_index, params.position)?;

    let result = snap.analysis.evaluate_predicate(params.text, FilePosition { file_id, offset })?;
    let status = match result.status {
        ide::PredicateEvaluationStatus::Holds => lsp_ext::PredicateEvaluationStatus::Holds,
        ide::PredicateEvaluationStatus::NotProven => lsp_ext::PredicateEvaluationStatus::NotProven,
        ide::PredicateEvaluationStatus::Invalid => lsp_ext::PredicateEvaluationStatus::Invalid,
        ide::PredicateEvaluationStatus::Unsupported => {
            lsp_ext::PredicateEvaluationStatus::Unsupported
        }
    };

    Ok(lsp_ext::EvaluatePredicateResult { status, message: result.message })
}

pub(crate) fn get_failed_obligations(
    snap: GlobalStateSnapshot,
    params: GetFailedObligationsParams,
) -> anyhow::Result<String> {
    let _p = tracing::info_span!("get_failed_obligations").entered();
    let file_id = try_default!(from_proto::file_id(&snap, &params.text_document.uri)?);
    let line_index = snap.file_line_index(file_id)?;
    let offset = from_proto::offset(&line_index, params.position)?;

    Ok(snap.analysis.get_failed_obligations(offset, file_id)?)
}

/// Searches for the directory of a Rust crate given this crate's root file path.
///
/// # Arguments
///
/// * `root_file_path`: The path to the root file of the crate.
///
/// # Returns
///
/// An `Option` value representing the path to the directory of the crate with the given
/// name, if such a crate is found. If no crate with the given name is found, this function
/// returns `None`.
fn crate_path(root_file_path: &VfsPath) -> Option<VfsPath> {
    let mut current_dir = root_file_path.parent();
    while let Some(path) = current_dir {
        let cargo_toml_path = path.join("../Cargo.toml")?;
        if fs::metadata(cargo_toml_path.as_path()?).is_ok() {
            let crate_path = cargo_toml_path.parent()?;
            return Some(crate_path);
        }
        current_dir = path.parent();
    }
    None
}

fn to_url(path: VfsPath) -> Option<Uri> {
    let path = path.as_path()?;
    let str_path = path.as_os_str().to_str()?;
    Uri::from_file_path(str_path).ok()
}

fn resource_ops_supported(config: &Config, kind: &DocumentChange) -> anyhow::Result<()> {
    let op = match kind {
        lsp_types::DocumentChange::CreateFile(_) => ResourceOperationKind::Create,
        lsp_types::DocumentChange::RenameFile(_) => ResourceOperationKind::Rename,
        lsp_types::DocumentChange::DeleteFile(_) => ResourceOperationKind::Delete,
        lsp_types::DocumentChange::TextDocumentEdit(_) => return Ok(()),
    };
    if !matches!(config.workspace_edit_resource_operations(), Some(resops) if resops.contains(&op))
    {
        return Err(LspError::new(
            ErrorCode::RequestFailed as i32,
            format!(
                "Client does not support {} capability.",
                match op {
                    ResourceOperationKind::Create => "create",
                    ResourceOperationKind::Rename => "rename",
                    ResourceOperationKind::Delete => "delete",
                    ResourceOperationKind::Custom(_) => unreachable!(),
                }
            ),
        )
        .into());
    }

    Ok(())
}

pub(crate) fn diff(left: &str, right: &str) -> TextEdit {
    use dissimilar::Chunk;

    let chunks = dissimilar::diff(left, right);

    let mut builder = TextEdit::builder();
    let mut pos = TextSize::default();

    let mut chunks = chunks.into_iter().peekable();
    while let Some(chunk) = chunks.next() {
        if let (Chunk::Delete(deleted), Some(&Chunk::Insert(inserted))) = (chunk, chunks.peek()) {
            chunks.next().unwrap();
            let deleted_len = TextSize::of(deleted);
            builder.replace(TextRange::at(pos, deleted_len), inserted.into());
            pos += deleted_len;
            continue;
        }

        match chunk {
            Chunk::Equal(text) => {
                pos += TextSize::of(text);
            }
            Chunk::Delete(deleted) => {
                let deleted_len = TextSize::of(deleted);
                builder.delete(TextRange::at(pos, deleted_len));
                pos += deleted_len;
            }
            Chunk::Insert(inserted) => {
                builder.insert(pos, inserted.into());
            }
        }
    }
    builder.finish()
}

#[test]
fn diff_smoke_test() {
    let mut original = String::from("fn foo(a:u32){\n}");
    let result = "fn foo(a: u32) {}";
    let edit = diff(&original, result);
    edit.apply(&mut original);
    assert_eq!(original, result);
}

#[cfg(test)]
fn ownership_preview_test_fix(
    uri: &Uri,
    strategy: OwnershipWrapperFix,
    edits: Vec<lsp_types::TextEdit>,
) -> Fix {
    let mut changes = FxHashMap::default();
    changes.insert(uri.clone(), edits);
    Fix {
        ranges: smallvec::smallvec![],
        action: lsp_ext::CodeAction {
            title: "Use Rc for shared ownership (compiler validated)".into(),
            group: None,
            kind: Some(lsp_types::CodeActionKind::QuickFix),
            command: None,
            edit: Some(lsp_ext::SnippetWorkspaceEdit {
                changes: Some(changes),
                document_changes: None,
                change_annotations: None,
            }),
            is_preferred: Some(false),
            data: None,
        },
        ownership_wrapper: Some(strategy),
    }
}

#[cfg(test)]
fn ownership_preview_test_line_index(source: &str) -> LineIndex {
    LineIndex {
        index: Arc::new(ide::LineIndex::new(source)),
        endings: LineEndings::Unix,
        encoding: crate::line_index::PositionEncoding::Utf8,
    }
}

#[test]
fn ownership_wrapper_preview_renders_multipart_diff() {
    let source = concat!(
        "fn main() {\n",
        "    let values: Box<Vec<i32>> = Box::new(vec![1]);\n",
        "    let shared = values;\n",
        "    println!(\"{} {}\", shared.len(), values.len());\n",
        "}\n",
    );
    let uri: Uri = "file:///test/src/main.rs".parse().unwrap();
    let fix = ownership_preview_test_fix(
        &uri,
        OwnershipWrapperFix::Rc,
        vec![
            lsp_types::TextEdit::new(
                Range::new(Position::new(1, 16), Position::new(1, 29)),
                "::std::rc::Rc<Vec<i32>>".into(),
            ),
            lsp_types::TextEdit::new(
                Range::new(Position::new(1, 32), Position::new(1, 40)),
                "::std::rc::Rc::new".into(),
            ),
            lsp_types::TextEdit::new(
                Range::new(Position::new(2, 23), Position::new(2, 23)),
                ".clone()".into(),
            ),
        ],
    );
    let diff =
        ownership_fix_diff(source, &ownership_preview_test_line_index(source), &uri, &fix).unwrap();
    assert!(diff.contains("-    let values: Box<Vec<i32>> = Box::new(vec![1]);"), "{diff}");
    assert!(
        diff.contains("+    let values: ::std::rc::Rc<Vec<i32>> = ::std::rc::Rc::new(vec![1]);"),
        "{diff}"
    );
    assert!(diff.contains("-    let shared = values;"), "{diff}");
    assert!(diff.contains("+    let shared = values.clone();"), "{diff}");
}

#[test]
fn ownership_wrapper_preview_rejects_overlapping_edits() {
    let source = "let value = Box::new(1);\n";
    let uri: Uri = "file:///test/src/main.rs".parse().unwrap();
    let fix = ownership_preview_test_fix(
        &uri,
        OwnershipWrapperFix::Rc,
        vec![
            lsp_types::TextEdit::new(
                Range::new(Position::new(0, 12), Position::new(0, 20)),
                "Rc::new".into(),
            ),
            lsp_types::TextEdit::new(
                Range::new(Position::new(0, 15), Position::new(0, 20)),
                "new".into(),
            ),
        ],
    );
    assert!(
        ownership_fix_diff(source, &ownership_preview_test_line_index(source), &uri, &fix,)
            .is_none()
    );
}

#[test]
fn ownership_wrapper_preview_caps_large_diffs() {
    let source = (0..14).map(|line| format!("value_{line}\n")).collect::<String>();
    let uri: Uri = "file:///test/src/main.rs".parse().unwrap();
    let edits = (0..14)
        .map(|line| {
            lsp_types::TextEdit::new(
                Range::new(Position::new(line, 0), Position::new(line, 5)),
                "shared".into(),
            )
        })
        .collect();
    let fix = ownership_preview_test_fix(&uri, OwnershipWrapperFix::Rc, edits);
    let diff = ownership_fix_diff(&source, &ownership_preview_test_line_index(&source), &uri, &fix)
        .unwrap();
    assert_eq!(diff.lines().count(), 24, "{diff}");
    assert!(diff.contains("preview truncated"), "{diff}");
}

#[test]
fn ownership_studio_reports_runtime_tradeoffs() {
    let effects = ownership_repair_effects(OwnershipWrapperFix::RcRefCell);
    assert_eq!(effects.thread_safety, "single-thread only");
    assert!(effects.runtime_risk.contains("panic"));
    assert!(effects.cost.contains("borrow-flag"));
}

#[test]
fn ownership_studio_c_sketch_is_explicitly_conceptual() {
    let sketch = ownership_c_sketch(Some("value"), &[], &[], &[]).unwrap();
    assert!(sketch.code.contains("temporary non-owning view"));
    assert!(sketch.code.contains("destroy(value)"));
    assert!(sketch.warning.contains("not Rust ABI-equivalent"));
    assert_eq!(sketch.provenance, "conceptual");
}

#[test]
fn ownership_problem_categories_are_structured_not_message_based() {
    let partial_move = OwnershipEvent {
        event_id: "partial".to_owned(),
        body_id: 7,
        basic_block: 1,
        statement_index: 0,
        kind: OwnershipEventKind::PartialMove,
        state: OwnershipState::PartiallyMoved,
        range: Range::new(Position::new(2, 4), Position::new(2, 13)),
        binding_range: Range::new(Position::new(0, 8), Position::new(0, 12)),
        name: "pair".to_owned(),
        place: "pair.left".to_owned(),
        loan_id: None,
        exact: true,
        detail: None,
        destination: None,
    };
    assert_eq!(ownership_problem_category("E0382", &[&partial_move]), "partial_move");
    assert_eq!(ownership_problem_category("E0382", &[]), "use_after_move");
    assert_eq!(ownership_problem_category("E0499", &[]), "multiple_mutable_borrows");
    assert_eq!(ownership_problem_category("E0502", &[]), "mutable_while_shared");
    assert_eq!(ownership_problem_category("E0505", &[]), "move_while_borrowed");
    assert_eq!(ownership_problem_category("E0506", &[]), "assign_while_borrowed");
    assert_eq!(ownership_problem_category("E0596", &[]), "immutable_mutation");
}

#[test]
fn ownership_problem_uses_compiler_binding_and_related_events() {
    let binding_range = Range::new(Position::new(1, 8), Position::new(1, 14));
    let move_range = Range::new(Position::new(2, 17), Position::new(2, 23));
    let invalid_range = Range::new(Position::new(4, 20), Position::new(4, 26));
    let events = vec![
        OwnershipEvent {
            event_id: "move".to_owned(),
            body_id: 9,
            basic_block: 1,
            statement_index: 0,
            kind: OwnershipEventKind::Move,
            state: OwnershipState::Moved,
            range: move_range,
            binding_range,
            name: "values".to_owned(),
            place: "values".to_owned(),
            loan_id: None,
            exact: true,
            detail: None,
            destination: None,
        },
        OwnershipEvent {
            event_id: "invalid".to_owned(),
            body_id: 0,
            basic_block: 0,
            statement_index: 0,
            kind: OwnershipEventKind::InvalidUse,
            state: OwnershipState::Moved,
            range: invalid_range,
            binding_range: invalid_range,
            name: "values".to_owned(),
            place: "values".to_owned(),
            loan_id: None,
            exact: true,
            detail: None,
            destination: None,
        },
    ];
    let tutorial = crate::diagnostics::OwnershipTutorialModel {
        schema_version: 3,
        bindings: vec![crate::diagnostics::OwnershipTutorialBinding {
            body_id: 9,
            name: "values".to_owned(),
            range: binding_range,
            type_name: "Box<Vec<i32>>".to_owned(),
            size: Some(8),
            align: Some(8),
            memory_layers: Vec::new(),
        }],
        ..Default::default()
    };
    let diagnostic = crate::diagnostics::OwnershipDiagnostic {
        code: "E0382".to_owned(),
        message: "borrow of moved value: `values`".to_owned(),
        range: invalid_range,
        related: Vec::new(),
    };
    let problem = ownership_problem_from_diagnostic(&diagnostic, &events, &tutorial);
    assert_eq!(problem.category, "use_after_move");
    assert_eq!(problem.binding_name, "values");
    assert_eq!(problem.binding_range, binding_range);
    assert_eq!(problem.primary_range, invalid_range);
    assert!(problem.related_ranges.contains(&move_range));
    assert_eq!(problem.precision, "compiler_exact");
}

#[test]
fn ownership_mutation_problem_does_not_collapse_field_target_to_self() {
    let diagnostic_range = Range::new(Position::new(13, 8), Position::new(14, 20));
    let self_range = Range::new(Position::new(12, 23), Position::new(12, 27));
    let events = vec![OwnershipEvent {
        event_id: "coarse-self-borrow".to_owned(),
        body_id: 1,
        basic_block: 0,
        statement_index: 0,
        kind: OwnershipEventKind::BorrowMutable,
        state: OwnershipState::MutablyBorrowed,
        range: diagnostic_range,
        binding_range: self_range,
        name: "self".to_owned(),
        place: "self.events".to_owned(),
        loan_id: Some(1),
        exact: true,
        detail: None,
        destination: None,
    }];
    let diagnostic = crate::diagnostics::OwnershipDiagnostic {
        code: "E0596".to_owned(),
        message: "cannot borrow `self.events` as mutable, as it is behind a `&` reference"
            .to_owned(),
        range: diagnostic_range,
        related: Vec::new(),
    };

    let problem = ownership_problem_from_diagnostic(
        &diagnostic,
        &events,
        &crate::diagnostics::OwnershipTutorialModel::default(),
    );

    assert_eq!(problem.category, "immutable_mutation");
    assert_eq!(problem.binding_name, "self.events");
    assert_ne!(problem.binding_name, "self");
}

#[test]
fn ownership_problem_protocol_round_trips_unicode_names() {
    let result = lsp_ext::OwnershipProblemsResult {
        schema_version: 3,
        status: "ready".to_owned(),
        source_hash: "abc".to_owned(),
        problems: vec![lsp_ext::OwnershipProblem {
            id: "E0596-данные".to_owned(),
            category: "immutable_mutation".to_owned(),
            diagnostic_code: Some("E0596".to_owned()),
            message: "cannot borrow `данные` as mutable".to_owned(),
            binding_name: "данные".to_owned(),
            primary_range: Range::new(Position::new(2, 4), Position::new(2, 10)),
            binding_range: Range::new(Position::new(0, 8), Position::new(0, 14)),
            related_ranges: Vec::new(),
            related: Vec::new(),
            model_position: Position::new(2, 4),
            precision: "compiler_exact".to_owned(),
        }],
    };
    let serialized = serde_json::to_string(&result).unwrap();
    let round_trip: lsp_ext::OwnershipProblemsResult = serde_json::from_str(&serialized).unwrap();
    assert_eq!(round_trip.problems[0].binding_name, "данные");
    assert!(round_trip.problems[0].message.contains("данные"));
}

#[test]
fn learning_problem_categories_cover_lifetimes_types_traits_closures_and_async() {
    for (code, expected) in [
        ("E0106", "missing_lifetime"),
        ("E0277", "trait_requirement"),
        ("E0308", "type_mismatch"),
        ("E0373", "closure_may_outlive_borrow"),
        ("E0507", "move_out_of_borrowed_content"),
        ("E0515", "returning_local_reference"),
        ("E0521", "borrowed_data_escapes"),
        ("E0597", "borrowed_value_too_short"),
        ("E0599", "method_or_trait_unavailable"),
        ("E0716", "temporary_dropped_while_borrowed"),
        ("E0728", "await_outside_async"),
        ("E0733", "recursive_async_function"),
    ] {
        assert_eq!(ownership_problem_category(code, &[]), expected, "wrong category for {code}");
    }
}

#[test]
fn ownership_repair_rejects_a_stale_source_hash() {
    let source = "fn main() {}\n";
    let current = stable_source_hash(source);
    assert!(ownership_repair_source_is_current(source, &current));
    assert!(!ownership_repair_source_is_current("fn main() { changed(); }\n", &current));
}

#[test]
fn ownership_value_trace_distinguishes_move_copy_and_rc_clone() {
    let range = Range::new(Position::new(2, 8), Position::new(2, 20));
    let binding_range = Range::new(Position::new(0, 8), Position::new(0, 20));
    let events = vec![
        lsp_ext::OwnershipModelEvent {
            event_id: "move".to_owned(),
            body_id: 1,
            basic_block: 0,
            statement_index: 2,
            kind: "move".to_owned(),
            state: "moved".to_owned(),
            place: "measurements".to_owned(),
            loan_id: None,
            range,
            binding_range,
            detail: None,
            destination: Some(lsp_ext::OwnershipModelEventDestination {
                kind: "local_binding".to_owned(),
                label: "chart_data".to_owned(),
                place: Some("chart_data".to_owned()),
                range: Some(range),
            }),
        },
        lsp_ext::OwnershipModelEvent {
            event_id: "copy".to_owned(),
            body_id: 1,
            basic_block: 0,
            statement_index: 3,
            kind: "copy".to_owned(),
            state: "available".to_owned(),
            place: "sample_count".to_owned(),
            loan_id: None,
            range,
            binding_range,
            detail: None,
            destination: None,
        },
    ];
    let operations = vec![lsp_ext::OwnershipOperationInsight {
        id: "rc-clone".to_owned(),
        range,
        name: "clone".to_owned(),
        signature: "fn clone(&self) -> Rc<T>".to_owned(),
        receiver_type: Some("Rc<Vec<i32>>".to_owned()),
        required_access: "shared_borrow".to_owned(),
        available_access: "shared".to_owned(),
        why_required: "creates another shared owner".to_owned(),
        documentation: None,
        effects: Vec::new(),
        effect_facts: Vec::new(),
        call_chain: vec!["clone".to_owned()],
        alternatives: Vec::new(),
        provenance: "resolved_signature".to_owned(),
        truncated: false,
    }];

    let trace = ownership_value_trace(&events, &operations);
    assert_eq!(trace[1].to_label.as_deref(), Some("chart_data"));
    assert!(trace[1].allocation_effect.contains("not copied"));
    assert_eq!(trace[2].source_state, "still available");
    assert!(trace[3].allocation_effect.contains("symbolic strong count"));
    assert_eq!(trace[3].destination_state.as_deref(), Some("new handle to the same allocation"));
}

#[test]
fn borrow_conflict_trace_keeps_borrower_referent_and_owner_distinct() {
    let range = Range::new(Position::new(4, 8), Position::new(4, 20));
    let graph = lsp_ext::OwnershipConflictGraph {
        title: "cannot replace current".to_owned(),
        summary: "prefix borrows current".to_owned(),
        requested_access: "replacement".to_owned(),
        nodes: vec![
            lsp_ext::OwnershipConflictNode {
                id: "borrower".to_owned(),
                label: "prefix".to_owned(),
                type_name: Some("&str".to_owned()),
                role: "borrower_reference".to_owned(),
                memory: "stack reference".to_owned(),
                range: Some(range),
            },
            lsp_ext::OwnershipConflictNode {
                id: "referent".to_owned(),
                label: "*current".to_owned(),
                type_name: Some("String".to_owned()),
                role: "borrowed_value".to_owned(),
                memory: "borrowed value".to_owned(),
                range: Some(range),
            },
            lsp_ext::OwnershipConflictNode {
                id: "owner".to_owned(),
                label: "self.events".to_owned(),
                type_name: Some("Vec<String>".to_owned()),
                role: "owner_path".to_owned(),
                memory: "owner path".to_owned(),
                range: Some(range),
            },
        ],
        edges: vec![lsp_ext::OwnershipConflictEdge {
            from: "borrower".to_owned(),
            to: "referent".to_owned(),
            kind: "borrows_shared".to_owned(),
            label: "holds a shared view into".to_owned(),
            provenance: "compiler_diagnostic".to_owned(),
        }],
        snapshots: vec![lsp_ext::OwnershipConflictSnapshot {
            phase: "borrow_created".to_owned(),
            title: "prefix starts borrowing".to_owned(),
            explanation: "prefix points into current".to_owned(),
            range,
            states: vec![
                lsp_ext::OwnershipConflictNodeState {
                    node_id: "borrower".to_owned(),
                    state: "alive · holds shared borrow".to_owned(),
                    explanation: "reference alive".to_owned(),
                },
                lsp_ext::OwnershipConflictNodeState {
                    node_id: "referent".to_owned(),
                    state: "alive · replacement blocked".to_owned(),
                    explanation: "value alive".to_owned(),
                },
            ],
        }],
        provenance: "compiler_diagnostic_and_source_semantics".to_owned(),
        truncated: false,
    };

    let trace = ownership_conflict_value_trace(&graph);
    assert_eq!(trace[0].from_label, "prefix");
    assert_eq!(trace[0].to_label.as_deref(), Some("*current"));
    assert!(trace[0].explanation.contains("self.events"));
    assert!(!trace[0].explanation.contains("`self` is the name rustc tracks"));
}

#[test]
fn bounded_ownership_model_protocol_marks_large_responses() {
    let model = lsp_ext::OwnershipModelResult {
        schema_version: 5,
        target_triple: "x86_64-unknown-linux-gnu".to_owned(),
        precision: "compiler_exact".to_owned(),
        status: "ready".to_owned(),
        truncated: true,
        source_hash: "abc".to_owned(),
        selected_problem_id: None,
        selected_place: None,
        events: Vec::new(),
        value_trace: Vec::new(),
        repairs: Vec::new(),
        bodies: Vec::new(),
        bindings: Vec::new(),
        loans: Vec::new(),
        memory_graph: lsp_ext::OwnershipModelMemoryGraph::default(),
        operations: vec![lsp_ext::OwnershipOperationInsight {
            id: "operation-0".to_owned(),
            range: Range::new(Position::new(2, 4), Position::new(2, 18)),
            name: "clear".to_owned(),
            signature: "fn clear(&mut self)".to_owned(),
            receiver_type: Some("Vec<i32>".to_owned()),
            required_access: "mutable_borrow".to_owned(),
            available_access: "exclusive path required".to_owned(),
            why_required: "the contents may change".to_owned(),
            documentation: None,
            effects: vec!["drops every element".to_owned()],
            effect_facts: vec![lsp_ext::OwnershipOperationEffect {
                kind: "destruction".to_owned(),
                summary: "drops every element".to_owned(),
                certainty: "trusted_standard_library_catalog".to_owned(),
            }],
            call_chain: vec!["clear".to_owned()],
            alternatives: vec![lsp_ext::OwnershipOperationAlternative {
                name: "truncate".to_owned(),
                signature: "truncate(n)".to_owned(),
                access: "mutable_borrow".to_owned(),
                behavior: "keeps a prefix".to_owned(),
                difference: "does not remove the prefix".to_owned(),
            }],
            provenance: "signature_docs_and_trusted_catalog".to_owned(),
            truncated: false,
        }],
        mutation_requirement: Some(lsp_ext::OwnershipMutationRequirement {
            target_place: "self.events".to_owned(),
            access_source: "&self".to_owned(),
            available_access: "shared_borrow".to_owned(),
            required_access: "mutable_borrow".to_owned(),
            operation_id: "operation-0".to_owned(),
            operation_name: "clear".to_owned(),
            explanation: "shared access cannot clear the field".to_owned(),
            provenance: "compiler_diagnostic_and_resolved_signature".to_owned(),
        }),
        conflict_graph: None,
        source_context: Some(lsp_ext::OwnershipSourceContext {
            file: "analytics.rs".to_owned(),
            breadcrumbs: vec![lsp_ext::OwnershipContextItem {
                kind: "function".to_owned(),
                label: "redact_latest".to_owned(),
                range: None,
            }],
            call_paths: vec![vec!["redact_latest".to_owned(), "last_mut".to_owned()]],
            related_types: vec!["Vec<String>".to_owned()],
            provenance: "source_syntax_resolved_calls_and_compiler_types".to_owned(),
            truncated: false,
        }),
        c_sketch: None,
    };
    let serialized = serde_json::to_string(&model).unwrap();
    assert!(serialized.contains("\"truncated\":true"));
    let round_trip: lsp_ext::OwnershipModelResult = serde_json::from_str(&serialized).unwrap();
    assert!(round_trip.truncated);
    assert_eq!(round_trip.operations[0].required_access, "mutable_borrow");
    assert_eq!(
        round_trip
            .mutation_requirement
            .as_ref()
            .map(|requirement| requirement.target_place.as_str()),
        Some("self.events")
    );
    assert_eq!(
        round_trip.source_context.as_ref().map(|context| context.file.as_str()),
        Some("analytics.rs")
    );
    assert_eq!(round_trip.operations[0].alternatives[0].name, "truncate");
}

#[test]
fn ownership_immutable_mutation_requirement_keeps_field_target_and_shared_self_distinct() {
    let source = concat!(
        "struct Events;\n",
        "impl Events { fn push(&mut self) {} }\n",
        "struct Analytics { events: Events }\n",
        "impl Analytics {\n",
        "    fn track(&self) {\n",
        "        self.events\n",
        "            .push();\n",
        "    }\n",
        "}\n",
    );
    let line_index = ownership_preview_test_line_index(source);
    let receiver_start = source.rfind("self.events").unwrap();
    let call_start = receiver_start;
    let call_end = source[call_start..].find(';').unwrap() + call_start;
    let diagnostic_range = to_proto::range(
        &line_index,
        TextRange::at(
            TextSize::from(receiver_start as u32),
            TextSize::from("self.events".len() as u32),
        ),
    );
    let operation_range = to_proto::range(
        &line_index,
        TextRange::new(TextSize::from(call_start as u32), TextSize::from(call_end as u32)),
    );
    let diagnostic = crate::diagnostics::OwnershipDiagnostic {
        code: "E0596".to_owned(),
        message: "cannot borrow `self.events` as mutable, as it is behind a `&` reference"
            .to_owned(),
        range: diagnostic_range,
        related: Vec::new(),
    };
    let operation = lsp_ext::OwnershipOperationInsight {
        id: "operation-push".to_owned(),
        range: operation_range,
        name: "push".to_owned(),
        signature: "fn push(&mut self)".to_owned(),
        receiver_type: Some("Events".to_owned()),
        required_access: "mutable_borrow".to_owned(),
        available_access: "shared access through self".to_owned(),
        why_required: "push may change the collection".to_owned(),
        documentation: None,
        effects: Vec::new(),
        effect_facts: Vec::new(),
        call_chain: vec!["push".to_owned()],
        alternatives: Vec::new(),
        provenance: "resolved_signature".to_owned(),
        truncated: false,
    };
    let file = syntax::SourceFile::parse(source, syntax::Edition::CURRENT).ok().unwrap();
    let requirement =
        ownership_mutation_requirement(Some(&diagnostic), Some(&file), &line_index, &[operation])
            .unwrap();

    assert_eq!(requirement.target_place, "self.events");
    assert_eq!(requirement.access_source, "&self");
    assert_eq!(requirement.available_access, "shared_borrow");
    assert_eq!(requirement.required_access, "mutable_borrow");
    assert_eq!(requirement.operation_name, "push");
    assert!(requirement.explanation.contains("`self.events`"));
    assert!(requirement.explanation.contains("`&self`"));
}

#[test]
fn ownership_immutable_mutation_requirement_follows_binding_to_later_call() {
    let source = concat!(
        "fn collect() {\n",
        "    let metrics: Vec<u32> = Vec::new();\n",
        "    metrics.push(60);\n",
        "}\n",
    );
    let line_index = ownership_preview_test_line_index(source);
    let binding_start = source.find("metrics: Vec").unwrap();
    let call_start = source.find("metrics.push").unwrap();
    let diagnostic_range = to_proto::range(
        &line_index,
        TextRange::at(TextSize::from(binding_start as u32), TextSize::from("metrics".len() as u32)),
    );
    let operation_range = to_proto::range(
        &line_index,
        TextRange::at(
            TextSize::from(call_start as u32),
            TextSize::from("metrics.push(60)".len() as u32),
        ),
    );
    let diagnostic = crate::diagnostics::OwnershipDiagnostic {
        code: "E0596".to_owned(),
        message: "cannot borrow `metrics` as mutable, as it is not declared as mutable".to_owned(),
        range: diagnostic_range,
        related: Vec::new(),
    };
    let operation = lsp_ext::OwnershipOperationInsight {
        id: "operation-push".to_owned(),
        range: operation_range,
        name: "push".to_owned(),
        signature: "fn push(&mut self, value: T)".to_owned(),
        receiver_type: Some("Vec<u32>".to_owned()),
        required_access: "mutable_borrow".to_owned(),
        available_access: "exclusive path required".to_owned(),
        why_required: "push may mutate the collection".to_owned(),
        documentation: None,
        effects: Vec::new(),
        effect_facts: Vec::new(),
        call_chain: vec!["push".to_owned()],
        alternatives: Vec::new(),
        provenance: "resolved_signature".to_owned(),
        truncated: false,
    };
    let file = syntax::SourceFile::parse(source, syntax::Edition::CURRENT).ok().unwrap();
    let requirement =
        ownership_mutation_requirement(Some(&diagnostic), Some(&file), &line_index, &[operation])
            .unwrap();

    assert_eq!(requirement.target_place, "metrics");
    assert_eq!(requirement.access_source, "immutable binding metrics");
    assert_eq!(requirement.available_access, "immutable_binding");
    assert_eq!(requirement.operation_name, "push");
}

#[test]
fn ownership_immutable_mutation_requirement_names_shared_owner_receiver() {
    let source = "fn update() { worker_cache.push(String::new()); }\n";
    let line_index = ownership_preview_test_line_index(source);
    let receiver_start = source.find("worker_cache").unwrap();
    let diagnostic_range = to_proto::range(
        &line_index,
        TextRange::at(
            TextSize::from(receiver_start as u32),
            TextSize::from("worker_cache".len() as u32),
        ),
    );
    let operation_range = to_proto::range(
        &line_index,
        TextRange::at(
            TextSize::from(receiver_start as u32),
            TextSize::from("worker_cache.push(String::new())".len() as u32),
        ),
    );
    let diagnostic = crate::diagnostics::OwnershipDiagnostic {
        code: "E0596".to_owned(),
        message: "cannot borrow data in an `Rc` as mutable".to_owned(),
        range: diagnostic_range,
        related: Vec::new(),
    };
    let operation = lsp_ext::OwnershipOperationInsight {
        id: "operation-push".to_owned(),
        range: operation_range,
        name: "push".to_owned(),
        signature: "fn push(&mut self, value: T)".to_owned(),
        receiver_type: Some("Rc<Vec<String>>".to_owned()),
        required_access: "mutable_borrow".to_owned(),
        available_access: "shared owner".to_owned(),
        why_required: "push may mutate the collection".to_owned(),
        documentation: None,
        effects: Vec::new(),
        effect_facts: Vec::new(),
        call_chain: vec!["push".to_owned()],
        alternatives: Vec::new(),
        provenance: "resolved_signature".to_owned(),
        truncated: false,
    };
    let file = syntax::SourceFile::parse(source, syntax::Edition::CURRENT).ok().unwrap();
    let requirement =
        ownership_mutation_requirement(Some(&diagnostic), Some(&file), &line_index, &[operation])
            .unwrap();

    assert_eq!(requirement.target_place, "worker_cache");
    assert_eq!(requirement.available_access, "shared_owner");
    assert!(requirement.access_source.contains("worker_cache"));
    assert!(requirement.explanation.contains("Rc<Vec<String>>"));
}

#[test]
fn ownership_model_protocol_accepts_pre_graph_responses() {
    let json = serde_json::json!({
        "schemaVersion": 5,
        "precision": "compiler_exact",
        "status": "ready",
        "truncated": false,
        "sourceHash": "abc",
        "selectedPlace": null,
        "events": [],
        "repairs": [],
        "bodies": [],
        "bindings": [],
        "loans": [],
        "operations": [],
        "cSketch": null
    });
    let model: lsp_ext::OwnershipModelResult = serde_json::from_value(json).unwrap();
    assert!(model.conflict_graph.is_none());
    assert!(model.value_trace.is_empty());
}

#[test]
fn ownership_metadata_only_inlay_data_does_not_require_resolution() {
    let metadata_only = serde_json::json!({
        "rustWorkbench": {
            "version": 1,
            "category": "ownership",
            "precision": "compiler_exact"
        }
    });
    assert!(rust_workbench_metadata_only_inlay_data(&metadata_only));

    let resolvable = serde_json::json!({
        "file_id": 7,
        "hash": "42",
        "resolve_range": {
            "start": { "line": 1, "character": 2 },
            "end": { "line": 1, "character": 3 }
        },
        "rustWorkbench": { "version": 1, "category": "lifetime" }
    });
    assert!(!rust_workbench_metadata_only_inlay_data(&resolvable));
}

#[test]
fn ownership_mechanics_hints_are_bounded_and_semantically_categorized() {
    let range = Range::new(Position::new(1, 8), Position::new(1, 14));
    let tutorial = crate::diagnostics::OwnershipTutorialModel {
        schema_version: 6,
        bindings: vec![crate::diagnostics::OwnershipTutorialBinding {
            body_id: 7,
            name: "values".to_owned(),
            range,
            type_name: "Box<Vec<i32>>".to_owned(),
            size: Some(8),
            align: Some(8),
            memory_layers: vec![crate::diagnostics::OwnershipModelMemoryLayer {
                kind: "box_allocation".to_owned(),
                storage: "heap".to_owned(),
                label: "Box allocation".to_owned(),
                type_name: "Vec<i32>".to_owned(),
                size: Some(24),
                align: Some(8),
                provenance: "compiler_exact".to_owned(),
            }],
        }],
        ..Default::default()
    };
    let hints = ownership_mechanics_hints(
        &tutorial,
        Range::new(Position::new(0, 0), Position::new(10, 0)),
        OwnershipMechanicsCategories { layout: true, storage: true, access: true, wrapper: true },
    );
    assert_eq!(hints.len(), 1, "all mechanics at one source anchor must merge");
    let metadata = &hints[0].data.as_ref().unwrap()["rustWorkbench"];
    assert_eq!(metadata["version"], 3);
    assert_eq!(metadata["category"], "mechanics");
    let categories = metadata["categories"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|category| category.as_str())
        .collect::<FxHashSet<_>>();
    assert!(categories.contains("layout"));
    assert!(categories.contains("storage"));
    assert!(categories.contains("wrapper"));
    assert_eq!(metadata["segments"].as_array().unwrap().len(), 3);
    assert!(hints.iter().all(|hint| hint.tooltip.is_none()));
    assert!(hints.iter().all(|hint| {
        hint.data.as_ref().unwrap()["rustWorkbenchTooltip"]
            .as_str()
            .is_some_and(|tooltip| !tooltip.is_empty())
    }));
    assert!(hints.len() <= 128);

    let layout_only = ownership_mechanics_hints(
        &tutorial,
        Range::new(Position::new(0, 0), Position::new(10, 0)),
        OwnershipMechanicsCategories {
            layout: true,
            storage: false,
            access: false,
            wrapper: false,
        },
    );
    assert_eq!(layout_only.len(), 1);
    assert_eq!(
        layout_only[0].data.as_ref().unwrap()["rustWorkbench"]["categories"],
        serde_json::json!(["layout"])
    );
}

#[test]
fn ownership_repair_preview_graph_is_bounded_and_honest_about_provenance() {
    let range = Range::new(Position::new(3, 8), Position::new(3, 14));
    let candidate =
        ownership_repair_preview_graph(OwnershipWrapperFix::ArcMutex, "values", range, false)
            .unwrap();
    assert!(candidate.nodes.len() <= 4);
    assert!(candidate.edges.len() <= 3);
    assert!(candidate.nodes.iter().any(|node| node.kind == "control_block"));
    assert!(candidate.nodes.iter().any(|node| node.kind == "lock_state"));
    assert!(candidate.nodes.iter().all(|node| node.provenance == "conceptual_candidate"));
    assert!(candidate.nodes.iter().all(|node| node.size.is_none()));

    let validated =
        ownership_repair_preview_graph(OwnershipWrapperFix::Rc, "values", range, true).unwrap();
    assert!(
        validated
            .nodes
            .iter()
            .all(|node| node.provenance == "derived_from_compiler_validated_rewrite")
    );
}

#[test]
fn ownership_conflict_graph_distinguishes_reference_handle_from_live_referent() {
    let source = concat!(
        "struct Analytics { events: Vec<String> }\n",
        "impl Analytics {\n",
        "    fn redact_latest(&mut self) {\n",
        "        let current = self.events.last_mut().expect(\"event\");\n",
        "        let prefix = &current[..current.find(':').unwrap_or(current.len())];\n",
        "        *current = String::from(\"redacted\");\n",
        "        println!(\"{prefix}\");\n",
        "    }\n",
        "}\n",
    );
    let line_index = ownership_preview_test_line_index(source);
    let range = |needle: &str, from_end: bool| {
        let byte_start =
            if from_end { source.rfind(needle).unwrap() } else { source.find(needle).unwrap() };
        to_proto::range(
            &line_index,
            TextRange::at(TextSize::from(byte_start as u32), TextSize::from(needle.len() as u32)),
        )
    };
    let borrow_range = range("&current[..current.find(':').unwrap_or(current.len())]", false);
    let assignment_range = range("*current", false);
    let last_use_range = range("prefix", true);
    let diagnostic = crate::diagnostics::OwnershipDiagnostic {
        code: "E0506".to_owned(),
        message: "cannot assign to `*current` because it is borrowed".to_owned(),
        range: assignment_range,
        related: vec![
            crate::diagnostics::OwnershipDiagnosticRelated {
                message: "`*current` is borrowed here".to_owned(),
                range: borrow_range,
            },
            crate::diagnostics::OwnershipDiagnosticRelated {
                message: "borrow later used here".to_owned(),
                range: last_use_range,
            },
        ],
    };
    let file = syntax::SourceFile::parse(source, syntax::Edition::CURRENT).ok().unwrap();
    let graph = ownership_conflict_graph(
        &diagnostic,
        &file,
        &line_index,
        &crate::diagnostics::OwnershipTutorialModel::default(),
        None,
    )
    .unwrap();

    assert!(graph.title.contains("`prefix`"));
    assert!(graph.title.contains("`*current`"));
    assert!(graph.nodes.iter().any(|node| node.label == "prefix"));
    assert!(graph.nodes.iter().any(|node| node.label == "current"));
    assert!(graph.nodes.iter().any(|node| node.label == "*current"));
    assert!(graph.nodes.iter().any(|node| node.label == "self.events"));
    assert!(graph.nodes.len() <= 32);
    assert!(graph.edges.len() <= 48);
    assert_eq!(graph.snapshots.len(), 3);
    assert!(
        graph
            .snapshots
            .iter()
            .flat_map(|snapshot| &snapshot.states)
            .all(|state| !state.state.contains("dead"))
    );
    assert!(graph.snapshots[2].explanation.contains("ends"));
}

#[test]
fn ownership_conflict_graph_keeps_shared_borrow_distinct_from_requested_write() {
    let source = concat!(
        "fn update(values: &mut Vec<String>) {\n",
        "    let view = &values[0];\n",
        "    values.push(String::new());\n",
        "    println!(\"{view}\");\n",
        "}\n",
    );
    let line_index = ownership_preview_test_line_index(source);
    let range = |needle: &str, from_end: bool| {
        let byte_start =
            if from_end { source.rfind(needle).unwrap() } else { source.find(needle).unwrap() };
        to_proto::range(
            &line_index,
            TextRange::at(TextSize::from(byte_start as u32), TextSize::from(needle.len() as u32)),
        )
    };
    let diagnostic = crate::diagnostics::OwnershipDiagnostic {
        code: "E0502".to_owned(),
        message: "cannot borrow `*values` as mutable because it is also borrowed as immutable"
            .to_owned(),
        range: range("values.push", false),
        related: vec![
            crate::diagnostics::OwnershipDiagnosticRelated {
                message: "immutable borrow occurs here".to_owned(),
                range: range("&values[0]", false),
            },
            crate::diagnostics::OwnershipDiagnosticRelated {
                message: "immutable borrow later used here".to_owned(),
                range: range("view", true),
            },
        ],
    };
    let file = syntax::SourceFile::parse(source, syntax::Edition::CURRENT).ok().unwrap();
    let graph = ownership_conflict_graph(
        &diagnostic,
        &file,
        &line_index,
        &crate::diagnostics::OwnershipTutorialModel::default(),
        None,
    )
    .unwrap();

    assert!(graph.title.contains("`view`"));
    assert_eq!(graph.requested_access, "mutable borrow");
    assert_eq!(graph.edges[0].kind, "borrows_shared");
    assert!(
        graph.snapshots[1].states.iter().any(|state| state.state == "alive · write access blocked")
    );
}

#[test]
fn ownership_field_diagnostic_keeps_exact_parent_context() {
    let binding_range = Range::new(Position::new(5, 8), Position::new(5, 12));
    let diagnostic_range = Range::new(Position::new(9, 4), Position::new(9, 13));
    let selected = OwnershipEvent {
        event_id: "diagnostic".to_owned(),
        body_id: 0,
        basic_block: 0,
        statement_index: 0,
        kind: crate::diagnostics::OwnershipEventKind::InvalidUse,
        state: OwnershipState::Moved,
        range: diagnostic_range,
        binding_range,
        name: "pair.left".to_owned(),
        place: "pair.left".to_owned(),
        loan_id: None,
        exact: false,
        detail: None,
        destination: None,
    };
    let tutorial = crate::diagnostics::OwnershipTutorialModel {
        schema_version: 3,
        bindings: vec![crate::diagnostics::OwnershipTutorialBinding {
            body_id: 42,
            name: "pair".to_owned(),
            range: binding_range,
            type_name: "Pair".to_owned(),
            size: None,
            align: None,
            memory_layers: Vec::new(),
        }],
        ..Default::default()
    };

    let (body_id, name) = ownership_selection_context(Some(&selected), &tutorial, diagnostic_range);
    assert_eq!(body_id, Some(42));
    assert_eq!(name.as_deref(), Some("pair"));

    let exact_parent_event = OwnershipEvent {
        event_id: "mir".to_owned(),
        body_id: 42,
        basic_block: 1,
        statement_index: 2,
        kind: crate::diagnostics::OwnershipEventKind::PartialMove,
        state: OwnershipState::PartiallyMoved,
        range: Range::new(Position::new(8, 4), Position::new(8, 13)),
        binding_range,
        name: "pair".to_owned(),
        place: "pair.0".to_owned(),
        loan_id: None,
        exact: true,
        detail: None,
        destination: None,
    };
    assert!(ownership_event_in_context(
        &exact_parent_event,
        Some(&selected),
        body_id,
        name.as_deref(),
    ));
    assert!(ownership_event_in_context(&selected, Some(&selected), body_id, name.as_deref(),));
}

#[test]
fn exact_ownership_events_add_semantic_modifiers() {
    let range = Range::new(Position::new(3, 4), Position::new(3, 9));
    let mut tokens = SemanticTokens {
        result_id: None,
        data: vec![lsp_types::SemanticToken {
            delta_line: 3,
            delta_start: 4,
            length: 5,
            token_type: 0,
            token_modifiers_bitset: 0,
        }],
    };
    enrich_ownership_tokens(
        &mut tokens,
        &[crate::diagnostics::OwnershipEvent {
            event_id: "event".to_owned(),
            body_id: 1,
            basic_block: 0,
            statement_index: 0,
            kind: crate::diagnostics::OwnershipEventKind::Move,
            state: crate::diagnostics::OwnershipState::Moved,
            range,
            binding_range: range,
            name: "value".to_owned(),
            place: "value".to_owned(),
            loan_id: None,
            exact: true,
            detail: None,
            destination: None,
        }],
    );
    assert_eq!(
        tokens.data[0].token_modifiers_bitset,
        crate::lsp::semantic_tokens::modifier_bit("moved").unwrap(),
    );
}
