mod learning_catalog;

use std::{collections::BTreeSet, path::PathBuf, time::Duration};

use db::kvp::KeyValueStore;
use editor::{
    Editor, EditorEvent, HighlightKey, RowHighlightOptions, RustInlineDiagnosticMode,
    RustOwnershipDisplayPreferences, RustOwnershipDisplayProfile, RustOwnershipHintScope,
};
use gpui::{
    AnyElement, App, AsyncWindowContext, Context, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, IntoElement, ParentElement, Pixels, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window, actions,
    prelude::*, px,
};
use language::{Buffer, point_from_lsp, point_to_lsp};
use project::{
    Project,
    lsp_store::rust_analyzer_ext::{
        self, OwnershipBinding, OwnershipLoanPoint, OwnershipModel, OwnershipProblem,
        OwnershipProblems,
    },
};
use text::{Bias, PointUtf16, ToPointUtf16};
use ui::{Button, Color, IconName, Label, LabelSize, prelude::*, utils::WithRemSize, v_flex};
use util::ResultExt as _;
use workspace::{
    ItemHandle, ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

actions!(
    rust_workbench,
    [
        /// Opens or focuses the Rust Learning Debugger.
        Toggle,
        /// Refreshes the compiler-backed Rust learning model.
        Refresh,
        /// Opens the Rust learning display profiles and hint filters.
        OpenDisplaySettings,
    ]
);

const PANEL_KEY: &str = "RustOwnershipWorkbench";
const DISPLAY_PREFERENCES_KEY_PREFIX: &str = "rust-workbench-display-v1";
const DEFAULT_PANEL_FONT_SCALE_PERCENT: u16 = 100;
const MIN_PANEL_FONT_SCALE_PERCENT: u16 = 80;
const MAX_PANEL_FONT_SCALE_PERCENT: u16 = 180;
const PANEL_FONT_SCALE_STEP_PERCENT: i16 = 10;
struct OwnershipStudioCue;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum LearningSection {
    Advanced,
}

impl LearningSection {
    fn title(self) -> &'static str {
        match self {
            Self::Advanced => "Explore deeper (optional)",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Self::Advanced => {
                "Memory, lifetimes, resolved calls, MIR evidence, and the optional C comparison"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CViewMode {
    #[default]
    Conceptual,
    Generated,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum CGenerationState {
    #[default]
    NotStarted,
    Waiting,
    Running,
    Ready,
    Blocked(SharedString),
    Failed(SharedString),
    Stale(SharedString),
}

#[derive(Clone, Debug)]
struct GeneratedCArtifact {
    code: SharedString,
    path: PathBuf,
    source_hash: String,
    backend: SharedString,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct IntentAnswers {
    multiple_owners: Option<bool>,
    mutation: Option<bool>,
    crosses_threads: Option<bool>,
    independent_clone: Option<bool>,
}

#[derive(Clone, Debug)]
struct RepairVerification {
    diagnostic_code: Option<String>,
    category: String,
    binding_name: String,
    original_line: u32,
    repair_title: String,
    baseline_problem_signatures: BTreeSet<String>,
    state: RepairVerificationState,
}

#[derive(Clone, Debug)]
enum RepairVerificationState {
    Applying,
    Checking,
    Resolved { remaining_file_problems: usize },
    IntroducedProblems { summaries: Vec<String> },
    StillPresent { current_line: u32 },
    Failed(SharedString),
}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &Toggle, window, cx| {
            if !workspace.toggle_panel_focus::<RustWorkbenchPanel>(window, cx) {
                workspace.close_panel::<RustWorkbenchPanel>(window, cx);
            }
        });
        workspace.register_action(|workspace, _: &Refresh, _window, cx| {
            if let Some(panel) = workspace.panel::<RustWorkbenchPanel>(cx) {
                panel.update(cx, |panel, cx| panel.schedule_refresh(cx));
            }
        });
        workspace.register_action(|workspace, _: &OpenDisplaySettings, window, cx| {
            workspace.open_panel::<RustWorkbenchPanel>(window, cx);
            if let Some(panel) = workspace.panel::<RustWorkbenchPanel>(cx) {
                panel.update(cx, |panel, cx| {
                    panel.show_display_controls = true;
                    cx.notify();
                });
            }
        });
    })
    .detach();
}

pub struct RustWorkbenchPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    active: bool,
    model: OwnershipModel,
    problems: OwnershipProblems,
    selected_problem_id: Option<String>,
    selection_epoch: u64,
    problem_status: SharedString,
    status_message: SharedString,
    active_buffer: Option<Entity<Buffer>>,
    active_position: Option<PointUtf16>,
    last_problem_key: Option<ProblemRequestKey>,
    pending_problem_key: Option<ProblemRequestKey>,
    last_model_key: Option<ModelRequestKey>,
    pending_model_key: Option<ModelRequestKey>,
    active_editor: Option<WeakEntity<Editor>>,
    editor_subscription: Option<Subscription>,
    refresh_task: Task<()>,
    problem_scan_task: Task<()>,
    problem_selection_task: Task<()>,
    font_scale_percent: u16,
    display_preferences: RustOwnershipDisplayPreferences,
    show_display_controls: bool,
    show_issue_list: bool,
    collapsed_sections: BTreeSet<LearningSection>,
    expanded_operations: BTreeSet<String>,
    repair_verification: Option<RepairVerification>,
    repair_validation_task: Task<()>,
    validating_repair_id: Option<String>,
    c_view_mode: CViewMode,
    c_generation_state: CGenerationState,
    generated_c: Option<GeneratedCArtifact>,
    generated_c_task: Task<()>,
    exact_mode: bool,
    visual_step: usize,
    preview_repair_id: Option<String>,
    show_repair_alternatives: bool,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModelRequestKey {
    source_hash: String,
    problem_id: Option<String>,
    position: PointUtf16,
    selection_epoch: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProblemRequestKey {
    buffer_id: EntityId,
    source_hash: String,
}

fn model_response_is_current(
    pending: Option<&ModelRequestKey>,
    request: &ModelRequestKey,
    selected_problem_id: Option<&str>,
    selection_epoch: u64,
) -> bool {
    pending == Some(request)
        && selection_epoch == request.selection_epoch
        && selected_problem_id == request.problem_id.as_deref()
}

pub struct OwnershipCoachBanner {
    workspace: WeakEntity<Workspace>,
    panel: Option<WeakEntity<RustWorkbenchPanel>>,
    panel_subscription: Option<Subscription>,
    problem_count: usize,
}

impl OwnershipCoachBanner {
    pub fn new(workspace: WeakEntity<Workspace>) -> Self {
        Self {
            workspace,
            panel: None,
            panel_subscription: None,
            problem_count: 0,
        }
    }

    fn connect_panel(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let Some(panel) = workspace.read(cx).panel::<RustWorkbenchPanel>(cx) else {
            return;
        };
        if self
            .panel
            .as_ref()
            .is_some_and(|current| current == &panel.downgrade())
        {
            defer_problem_scan(&panel, cx);
            self.sync_from_panel(&panel, cx);
            return;
        }
        self.panel = Some(panel.downgrade());
        self.panel_subscription = Some(cx.observe(&panel, |banner, panel, cx| {
            banner.sync_from_panel(&panel, cx);
        }));
        // Pane restoration calls toolbar items while the Pane entity itself is leased for update.
        // Looking up the active editor synchronously from here would try to re-read that Pane and
        // panic. The deferred effect runs after the restoration/update cycle releases its lease.
        defer_problem_scan(&panel, cx);
        self.sync_from_panel(&panel, cx);
    }

    fn sync_from_panel(&mut self, panel: &Entity<RustWorkbenchPanel>, cx: &mut Context<Self>) {
        let problem_count = panel.read(cx).ownership_problems().len();
        if problem_count == self.problem_count {
            return;
        }
        self.problem_count = problem_count;
        cx.emit(ToolbarItemEvent::ChangeLocation(if problem_count == 0 {
            ToolbarItemLocation::Hidden
        } else {
            ToolbarItemLocation::Secondary
        }));
        cx.notify();
    }
}

fn defer_problem_scan(panel: &Entity<RustWorkbenchPanel>, cx: &mut Context<OwnershipCoachBanner>) {
    let panel = panel.downgrade();
    cx.defer(move |cx| {
        if let Some(panel) = panel.upgrade() {
            panel.update(cx, |panel, cx| panel.ensure_problem_scan(cx));
        }
    });
}

impl EventEmitter<ToolbarItemEvent> for OwnershipCoachBanner {}

impl ToolbarItemView for OwnershipCoachBanner {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        if active_pane_item
            .and_then(|item| item.act_as::<Editor>(cx))
            .is_none()
        {
            self.problem_count = 0;
            return ToolbarItemLocation::Hidden;
        }
        self.connect_panel(cx);
        if self.problem_count == 0 {
            ToolbarItemLocation::Hidden
        } else {
            ToolbarItemLocation::Secondary
        }
    }
}

impl Render for OwnershipCoachBanner {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.problem_count;
        h_flex()
            .w_full()
            .py_1()
            .px_2()
            .justify_between()
            .bg(cx.theme().status().error_background.opacity(0.45))
            .border_1()
            .border_color(cx.theme().status().error)
            .rounded_sm()
            .child(
                Label::new(if count == 1 {
                    "Explainable Rust issue detected".to_owned()
                } else {
                    format!("{count} explainable Rust issues detected")
                })
                .size(LabelSize::Small)
                .color(Color::Error),
            )
            .child(
                Button::new("explain-ownership-visually", "Explain visually")
                    .on_click(|_event, window, cx| window.dispatch_action(Box::new(Toggle), cx)),
            )
    }
}

impl RustWorkbenchPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let project = workspace.project().clone();
            let workspace_id = workspace.database_id();
            let display_preferences = workspace_id
                .and_then(|workspace_id| {
                    let key = format!("{DISPLAY_PREFERENCES_KEY_PREFIX}:{workspace_id:?}");
                    KeyValueStore::global(cx).read_kvp(&key).ok().flatten()
                })
                .and_then(|serialized| serde_json::from_str(&serialized).ok())
                .unwrap_or_else(RustOwnershipDisplayPreferences::focus);
            let workspace_entity = cx.entity();
            let workspace_handle = workspace_entity.downgrade();
            cx.new(|cx| {
                let workspace_subscription = cx.subscribe_in(
                    &workspace_entity,
                    window,
                    |panel: &mut RustWorkbenchPanel, _, event, _window, cx| {
                        if matches!(event, workspace::Event::ActiveItemChanged) {
                            panel.last_model_key = None;
                            panel.pending_model_key = None;
                            panel.observe_active_editor(cx);
                            panel.schedule_problem_scan(cx);
                        }
                    },
                );
                let project_subscription = cx.subscribe(&project, |panel, _, event, cx| {
                    if let project::Event::OwnershipModelChanged { uri, .. } = event
                        && panel.owns_active_uri(uri, cx)
                    {
                        panel.last_model_key = None;
                        panel.pending_model_key = None;
                        panel.last_problem_key = None;
                        panel.pending_problem_key = None;
                        panel.validating_repair_id = None;
                        panel.schedule_problem_scan(cx);
                    }
                });
                Self {
                    workspace: workspace_handle,
                    project,
                    focus_handle: cx.focus_handle(),
                    active: false,
                    model: OwnershipModel::default(),
                    problems: OwnershipProblems::default(),
                    selected_problem_id: None,
                    selection_epoch: 0,
                    problem_status: "Checking this file for explainable Rust problems…".into(),
                    status_message: "Select a Rust variable, then refresh.".into(),
                    active_buffer: None,
                    active_position: None,
                    last_problem_key: None,
                    pending_problem_key: None,
                    last_model_key: None,
                    pending_model_key: None,
                    active_editor: None,
                    editor_subscription: None,
                    refresh_task: Task::ready(()),
                    problem_scan_task: Task::ready(()),
                    problem_selection_task: Task::ready(()),
                    font_scale_percent: DEFAULT_PANEL_FONT_SCALE_PERCENT,
                    display_preferences,
                    show_display_controls: false,
                    show_issue_list: false,
                    collapsed_sections: BTreeSet::from([LearningSection::Advanced]),
                    expanded_operations: BTreeSet::new(),
                    repair_verification: None,
                    repair_validation_task: Task::ready(()),
                    validating_repair_id: None,
                    c_view_mode: CViewMode::Conceptual,
                    c_generation_state: CGenerationState::NotStarted,
                    generated_c: None,
                    generated_c_task: Task::ready(()),
                    exact_mode: false,
                    visual_step: 0,
                    preview_repair_id: None,
                    show_repair_alternatives: false,
                    _subscriptions: vec![workspace_subscription, project_subscription],
                }
            })
        })
    }

    fn observe_active_editor(&mut self, cx: &mut Context<Self>) {
        let editor = self
            .workspace
            .upgrade()
            .and_then(|workspace| workspace.read(cx).active_item_as::<Editor>(cx));
        if editor.as_ref().map(Entity::downgrade) == self.active_editor {
            self.apply_display_to_editor(cx);
            return;
        }
        self.clear_editor_cue(cx);
        self.last_model_key = None;
        self.pending_model_key = None;
        self.last_problem_key = None;
        self.pending_problem_key = None;
        self.problems = OwnershipProblems::default();
        self.selected_problem_id = None;
        self.selection_epoch = self.selection_epoch.wrapping_add(1);
        self.model = OwnershipModel::default();
        self.preview_repair_id = None;
        self.show_repair_alternatives = false;
        self.repair_validation_task = Task::ready(());
        self.validating_repair_id = None;
        if let Some(previous_editor) = self.active_editor.as_ref().and_then(WeakEntity::upgrade) {
            previous_editor.update(cx, |editor, cx| {
                editor.set_rust_ownership_display_preferences(
                    RustOwnershipDisplayPreferences::default(),
                    cx,
                );
            });
        }
        self.active_editor = editor.as_ref().map(Entity::downgrade);
        self.editor_subscription = editor.map(|editor| {
            cx.subscribe(&editor, |panel, _, event, cx| {
                let source_changed = matches!(
                    event,
                    EditorEvent::Edited { .. }
                        | EditorEvent::BufferEdited
                        | EditorEvent::Reparsed(_)
                        | EditorEvent::FileHandleChanged
                );
                if source_changed {
                    panel.last_model_key = None;
                    panel.pending_model_key = None;
                    panel.last_problem_key = None;
                    panel.pending_problem_key = None;
                    panel.repair_validation_task = Task::ready(());
                    panel.validating_repair_id = None;
                    panel.clear_editor_cue(cx);
                    if let Some(verification) = &mut panel.repair_verification
                        && !matches!(
                            verification.state,
                            RepairVerificationState::Applying | RepairVerificationState::Checking
                        )
                    {
                        verification.state = RepairVerificationState::Checking;
                    }
                    panel.mark_generated_c_stale("Source changed; save to regenerate.", cx);
                    panel.schedule_problem_scan(cx);
                }
                if panel.active
                    && panel.c_view_mode == CViewMode::Generated
                    && !panel
                        .collapsed_sections
                        .contains(&LearningSection::Advanced)
                    && matches!(event, EditorEvent::Saved)
                {
                    panel.schedule_generated_c(cx);
                }
                if panel.active && matches!(event, EditorEvent::SelectionsChanged { local: true }) {
                    panel.schedule_cursor_problem_selection(cx);
                }
            })
        });
        self.apply_display_to_editor(cx);
    }

    fn owns_active_uri(&self, uri: &lsp::Uri, cx: &App) -> bool {
        let Ok(notification_path) = uri.to_file_path() else {
            return false;
        };
        self.active_buffer.as_ref().is_some_and(|buffer| {
            buffer
                .read(cx)
                .file()
                .and_then(|file| file.as_local())
                .is_some_and(|file| file.abs_path(cx) == notification_path)
        })
    }

    fn clear_editor_cue(&self, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        editor.update(cx, |editor, cx| {
            editor.clear_background_highlights(HighlightKey::RustOwnershipWorkbench, cx);
            editor.clear_row_highlights::<OwnershipStudioCue>();
            cx.notify();
        });
    }

    pub fn ownership_problems(&self) -> &[OwnershipProblem] {
        &self.problems.problems
    }

    pub fn problem_status(&self) -> &str {
        &self.problem_status
    }

    fn selected_problem(&self) -> Option<&OwnershipProblem> {
        let selected = self.selected_problem_id.as_deref()?;
        self.problems
            .problems
            .iter()
            .find(|problem| problem.id == selected)
    }

    fn selected_problem_index(&self) -> Option<usize> {
        let selected = self.selected_problem_id.as_deref()?;
        self.problems
            .problems
            .iter()
            .position(|problem| problem.id == selected)
    }

    fn active_cursor_position(&self, cx: &App) -> Option<lsp::Position> {
        let editor = self.active_editor.as_ref()?.upgrade()?;
        editor.read_with(cx, |editor, cx| {
            let multibuffer = editor.buffer().read(cx);
            let snapshot = multibuffer.snapshot(cx);
            let (anchor, buffer_snapshot) =
                snapshot.anchor_to_buffer_anchor(editor.selections.newest_anchor().head())?;
            Some(point_to_lsp(anchor.to_point_utf16(buffer_snapshot)))
        })
    }

    fn problem_index_at_cursor(&self, cx: &App) -> Option<usize> {
        ownership_problem_index_at_position(
            &self.problems.problems,
            self.active_cursor_position(cx)?,
        )
    }

    fn select_problem_index(&mut self, index: usize, reveal_source: bool, cx: &mut Context<Self>) {
        let Some(problem) = self.problems.problems.get(index) else {
            return;
        };
        let id = problem.id.clone();
        let range = problem.primary_range;
        let changed = self.selected_problem_id.as_deref() != Some(id.as_str());
        if !changed {
            if reveal_source {
                self.cue_range(range, cx);
            }
            return;
        }
        self.selected_problem_id = Some(id);
        self.selection_epoch = self.selection_epoch.wrapping_add(1);
        self.last_model_key = None;
        self.pending_model_key = None;
        self.model = OwnershipModel::default();
        self.repair_verification = None;
        self.repair_validation_task = Task::ready(());
        self.validating_repair_id = None;
        self.status_message = "Loading compiler facts for the selected issue…".into();
        self.visual_step = 0;
        self.preview_repair_id = None;
        self.show_repair_alternatives = false;
        if reveal_source {
            self.cue_range(range, cx);
        }
        self.apply_display_to_editor(cx);
        self.schedule_refresh(cx);
        cx.notify();
    }

    fn select_relative_problem(&mut self, direction: isize, cx: &mut Context<Self>) {
        let Some(index) = relative_problem_index(
            self.selected_problem_index(),
            self.problems.problems.len(),
            direction,
        ) else {
            return;
        };
        self.select_problem_index(index, true, cx);
    }

    fn select_visual_step(&mut self, index: usize, cx: &mut Context<Self>) {
        let range = self
            .model
            .value_trace
            .get(index)
            .map(|step| step.range)
            .or_else(|| {
                visual_moments(self.selected_problem(), &self.model)
                    .get(index)
                    .map(|moment| moment.range)
            });
        let Some(range) = range else {
            return;
        };
        self.visual_step = index;
        self.cue_range(range, cx);
        cx.notify();
    }

    fn preview_repair(&mut self, repair_id: String, cx: &mut Context<Self>) {
        if self.preview_repair_id.as_deref() == Some(repair_id.as_str()) {
            self.preview_repair_id = None;
            cx.notify();
            return;
        }
        self.preview_repair_id = Some(repair_id.clone());

        let Some(repair) = self
            .model
            .repairs
            .iter()
            .find(|repair| repair.id == repair_id)
        else {
            cx.notify();
            return;
        };
        if repair_is_compiler_validated(repair) {
            cx.notify();
            return;
        }
        if self.validating_repair_id.as_deref() == Some(repair_id.as_str()) {
            self.status_message =
                "rustc is already checking this candidate. Apply appears only if it passes.".into();
            cx.notify();
            return;
        }
        let (Some(buffer), Some(position)) = (self.active_buffer.clone(), self.active_position)
        else {
            self.status_message = "Open a saved Rust file before validating this candidate.".into();
            cx.notify();
            return;
        };
        let validation = rust_analyzer_ext::ownership_validate_repair(
            self.project.clone(),
            buffer,
            position,
            repair_id.clone(),
            self.model.source_hash.clone(),
            cx,
        );
        self.validating_repair_id = Some(repair_id.clone());
        self.status_message =
            "Preview ready. rustc is checking whether the complete rewrite still compiles…".into();
        self.repair_validation_task = cx.spawn(async move |panel, cx| {
            let result = validation.await;
            panel
                .update(cx, |panel, cx| {
                    if panel.validating_repair_id.as_deref() != Some(repair_id.as_str()) {
                        return;
                    }
                    match result {
                        Ok(result) => {
                            if result.status != "checking" {
                                panel.validating_repair_id = None;
                            }
                            panel.status_message = result.message.into();
                        }
                        Err(error) => {
                            panel.validating_repair_id = None;
                            panel.status_message =
                                format!("Could not start compiler validation: {error}").into();
                        }
                    }
                    cx.notify();
                })
                .ok();
        });
        cx.notify();
    }

    fn adjust_font_scale(&mut self, delta_percent: i16, cx: &mut Context<Self>) {
        self.font_scale_percent = adjusted_panel_font_scale(self.font_scale_percent, delta_percent);
        cx.notify();
    }

    fn toggle_learning_section(&mut self, section: LearningSection, cx: &mut Context<Self>) {
        if !self.collapsed_sections.remove(&section) {
            self.collapsed_sections.insert(section);
            if section == LearningSection::Advanced {
                self.cancel_generated_c(cx);
            }
        } else if section == LearningSection::Advanced && self.c_view_mode == CViewMode::Generated {
            self.schedule_generated_c(cx);
        }
        cx.notify();
    }

    fn toggle_operation_details(&mut self, operation_id: String, cx: &mut Context<Self>) {
        if !self.expanded_operations.remove(&operation_id) {
            self.expanded_operations.insert(operation_id);
        }
        cx.notify();
    }

    fn update_repair_verification(
        &mut self,
        problems: &OwnershipProblems,
        _cx: &mut Context<Self>,
    ) {
        let Some(verification) = &self.repair_verification else {
            return;
        };
        if !matches!(verification.state, RepairVerificationState::Checking) {
            return;
        }
        let outcome = repair_verification_outcome(verification, problems);
        if let Some(verification) = &mut self.repair_verification {
            verification.state = outcome;
        }
    }

    fn set_display_profile(
        &mut self,
        profile: RustOwnershipDisplayProfile,
        cx: &mut Context<Self>,
    ) {
        self.display_preferences = match profile {
            RustOwnershipDisplayProfile::Focus => RustOwnershipDisplayPreferences::focus(),
            RustOwnershipDisplayProfile::Learn => RustOwnershipDisplayPreferences::learn(),
            RustOwnershipDisplayProfile::Full => RustOwnershipDisplayPreferences::full(),
            RustOwnershipDisplayProfile::Custom => self.display_preferences.clone(),
        };
        self.display_preferences_changed(cx);
    }

    fn toggle_display_filter(&mut self, filter: &'static str, cx: &mut Context<Self>) {
        let preferences = &mut self.display_preferences;
        match filter {
            "types" => preferences.show_type_hints = !preferences.show_type_hints,
            "parameters" => preferences.show_parameter_hints = !preferences.show_parameter_hints,
            "other" => preferences.show_other_hints = !preferences.show_other_hints,
            "adjustments" => preferences.show_adjustments = !preferences.show_adjustments,
            "lifetimes" => preferences.show_lifetimes = !preferences.show_lifetimes,
            "moves" => preferences.show_moves = !preferences.show_moves,
            "borrows" => preferences.show_borrows = !preferences.show_borrows,
            "invalid_uses" => preferences.show_invalid_uses = !preferences.show_invalid_uses,
            "last_uses" => preferences.show_last_uses = !preferences.show_last_uses,
            "borrow_ends" => preferences.show_borrow_ends = !preferences.show_borrow_ends,
            "reinitializations" => {
                preferences.show_reinitializations = !preferences.show_reinitializations
            }
            "drops" => preferences.show_drops = !preferences.show_drops,
            "ownership_colors" => {
                preferences.show_ownership_coloring = !preferences.show_ownership_coloring
            }
            _ => return,
        }
        preferences.profile = RustOwnershipDisplayProfile::Custom;
        self.display_preferences_changed(cx);
    }

    fn display_preferences_changed(&mut self, cx: &mut Context<Self>) {
        self.persist_display_preferences(cx);
        self.apply_display_to_editor(cx);
        cx.notify();
    }

    fn persist_display_preferences(&self, cx: &mut Context<Self>) {
        let Some(workspace_id) = self
            .workspace
            .upgrade()
            .and_then(|workspace| workspace.read(cx).database_id())
        else {
            return;
        };
        let Ok(serialized) = serde_json::to_string(&self.display_preferences) else {
            return;
        };
        let key = format!("{DISPLAY_PREFERENCES_KEY_PREFIX}:{workspace_id:?}");
        let database = KeyValueStore::global(cx);
        db::write_and_log(cx, move || async move {
            database.write_kvp(key, serialized).await
        });
    }

    fn display_focus_rows(&self, cx: &App) -> Vec<(u32, u32)> {
        if self.display_preferences.scope == RustOwnershipHintScope::File {
            return Vec::new();
        }
        let mut rows = Vec::new();
        if let Some(problem) = self.selected_problem() {
            rows.push((
                problem.binding_range.start.line,
                problem.binding_range.end.line,
            ));
            rows.push((
                problem.primary_range.start.line,
                problem.primary_range.end.line,
            ));
            rows.extend(
                problem
                    .related_ranges
                    .iter()
                    .map(|range| (range.start.line, range.end.line)),
            );
        }
        rows.extend(
            self.model
                .events
                .iter()
                .map(|event| (event.range.start.line, event.range.end.line)),
        );
        if self.display_preferences.scope == RustOwnershipHintScope::CurrentFunction {
            if let Some(function_rows) = self.current_rust_function_rows(cx) {
                return vec![function_rows];
            }
            if let (Some(start), Some(end)) = (
                rows.iter().map(|(start, _)| *start).min(),
                rows.iter().map(|(_, end)| *end).max(),
            ) {
                return vec![(start.saturating_sub(1), end.saturating_add(1))];
            }
        }
        rows
    }

    fn current_rust_function_rows(&self, cx: &App) -> Option<(u32, u32)> {
        let buffer = self.active_buffer.as_ref()?;
        let position = self
            .selected_problem()
            .map(|problem| problem.primary_range.start)
            .or_else(|| self.active_cursor_position(cx))?;
        let snapshot = buffer.read(cx).snapshot();
        let point_utf16 = snapshot.clip_point_utf16(point_from_lsp(position), Bias::Left);
        let point = snapshot.point_utf16_to_point(point_utf16);
        let offset = snapshot.point_to_offset(point);
        let mut node = snapshot.syntax_ancestor(offset..offset)?;
        loop {
            if node.kind() == "function_item" {
                let start = u32::try_from(node.start_position().row).ok()?;
                let end = u32::try_from(node.end_position().row).ok()?;
                return Some((start, end));
            }
            node = node.parent()?;
        }
    }

    fn apply_display_to_editor(&self, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        let mut preferences = self.display_preferences.clone();
        preferences.enabled = true;
        preferences.focus_rows = self.display_focus_rows(cx);
        editor.update(cx, |editor, cx| {
            editor.set_rust_ownership_display_preferences(preferences, cx);
        });
    }

    fn mark_generated_c_stale(&mut self, reason: &'static str, cx: &mut Context<Self>) {
        self.generated_c_task = Task::ready(());
        if self.generated_c.is_some() {
            self.c_generation_state = CGenerationState::Stale(reason.into());
        } else if self.c_view_mode == CViewMode::Generated {
            self.c_generation_state = CGenerationState::Waiting;
        }
        cx.notify();
    }

    fn schedule_generated_c(&mut self, cx: &mut Context<Self>) {
        if self.c_view_mode != CViewMode::Generated
            || self.collapsed_sections.contains(&LearningSection::Advanced)
        {
            return;
        }
        let Some(buffer) = self.active_buffer.clone() else {
            self.c_generation_state =
                CGenerationState::Blocked("Open a saved Rust file first.".into());
            cx.notify();
            return;
        };
        if buffer.read(cx).is_dirty() {
            self.c_generation_state = CGenerationState::Blocked(
                "Generated C requires saved Rust. Save the file, then refresh.".into(),
            );
            cx.notify();
            return;
        }
        let Some(source_path) = buffer
            .read(cx)
            .file()
            .and_then(|file| file.as_local())
            .map(|file| file.abs_path(cx))
        else {
            self.c_generation_state = CGenerationState::Blocked(
                "Generated C is available for local Cargo projects only.".into(),
            );
            cx.notify();
            return;
        };
        let source = buffer.read(cx).snapshot().text();
        let source_hash = ownership_source_hash(&source);
        self.c_generation_state = CGenerationState::Waiting;
        cx.notify();
        self.generated_c_task = cx.spawn(async move |panel, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            panel
                .update(cx, |panel, cx| {
                    panel.c_generation_state = CGenerationState::Running;
                    cx.notify();
                })
                .ok();
            let generation = cx.background_spawn(generate_c_artifact(source_path, source_hash));
            let result = generation.await;
            panel
                .update(cx, |panel, cx| {
                    match result {
                        Ok(artifact) => {
                            let source_is_current =
                                panel.active_buffer.as_ref().is_some_and(|buffer| {
                                    ownership_source_hash(&buffer.read(cx).snapshot().text())
                                        == artifact.source_hash
                                });
                            panel.c_generation_state = if source_is_current {
                                CGenerationState::Ready
                            } else {
                                CGenerationState::Stale(
                                    "Rust changed while C was being generated.".into(),
                                )
                            };
                            panel.generated_c = Some(artifact);
                        }
                        Err(error) => {
                            let message = error.to_string();
                            panel.c_generation_state = if message.contains("rustic toolchain") {
                                CGenerationState::Blocked(message.into())
                            } else {
                                CGenerationState::Failed(message.into())
                            };
                        }
                    }
                    cx.notify();
                })
                .ok();
        });
    }

    fn cancel_generated_c(&mut self, cx: &mut Context<Self>) {
        self.generated_c_task = Task::ready(());
        self.c_generation_state = if self.generated_c.is_some() {
            CGenerationState::Stale("Generation cancelled; showing the previous artifact.".into())
        } else {
            CGenerationState::NotStarted
        };
        cx.notify();
    }

    fn open_generated_c(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self
            .generated_c
            .as_ref()
            .map(|artifact| artifact.path.clone())
        else {
            return;
        };
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_panel, cx| {
            let open = workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.open_abs_path(path, workspace::OpenOptions::default(), window, cx)
                })
                .ok();
            if let Some(open) = open {
                open.await.log_err();
            }
        })
        .detach();
    }

    fn schedule_cursor_problem_selection(&mut self, cx: &mut Context<Self>) {
        self.problem_selection_task = cx.spawn(async move |panel, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(40))
                .await;
            panel
                .update(cx, |panel, cx| {
                    if panel.problems.problems.is_empty() {
                        if panel.pending_problem_key.is_none() {
                            panel.schedule_problem_scan(cx);
                        }
                    } else if let Some(index) = panel.problem_index_at_cursor(cx) {
                        panel.select_problem_index(index, false, cx);
                    }
                })
                .ok();
        });
    }

    pub fn ensure_problem_scan(&mut self, cx: &mut Context<Self>) {
        let previous_editor = self.active_editor.clone();
        self.observe_active_editor(cx);
        if previous_editor != self.active_editor
            || self.last_problem_key.is_none() && self.pending_problem_key.is_none()
        {
            self.schedule_problem_scan(cx);
        }
    }

    fn schedule_problem_scan(&mut self, cx: &mut Context<Self>) {
        self.preview_repair_id = None;
        self.visual_step = 0;
        self.problem_status = "Checking this file for explainable Rust problems…".into();
        cx.notify();
        self.problem_scan_task = cx.spawn(async move |panel, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            panel.update(cx, |panel, cx| panel.scan_problems(cx)).ok();
        });
    }

    fn scan_problems(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor.as_ref().and_then(WeakEntity::upgrade) else {
            self.problem_status = "Open a Rust source file to use the ownership coach.".into();
            self.problems = OwnershipProblems::default();
            cx.notify();
            return;
        };
        let Some(buffer) = editor.read_with(cx, |editor, cx| {
            let multibuffer = editor.buffer().read(cx);
            let snapshot = multibuffer.snapshot(cx);
            let (anchor, _) =
                snapshot.anchor_to_buffer_anchor(editor.selections.newest_anchor().head())?;
            multibuffer.buffer(anchor.buffer_id)
        }) else {
            self.problem_status = "Open a Rust source file to use the ownership coach.".into();
            self.problems = OwnershipProblems::default();
            cx.notify();
            return;
        };
        self.active_buffer = Some(buffer.clone());
        let source = buffer.read(cx).snapshot().text();
        let request_key = ProblemRequestKey {
            buffer_id: buffer.entity_id(),
            source_hash: ownership_source_hash(&source),
        };
        if self.pending_problem_key.as_ref() == Some(&request_key)
            || self.last_problem_key.as_ref() == Some(&request_key)
        {
            return;
        }
        self.pending_problem_key = Some(request_key.clone());
        let requested_buffer = buffer.clone();
        let request = rust_analyzer_ext::ownership_problems(self.project.clone(), buffer, cx);
        self.problem_scan_task = cx.spawn(async move |panel, cx| {
            let result = request.await;
            panel
                .update(cx, |panel, cx| {
                    // A scan may finish after the user changed files, edited the source, or
                    // explicitly selected another issue. Never let that late response replace
                    // the current issue list or trigger a model request for the wrong problem.
                    if panel.pending_problem_key.as_ref() != Some(&request_key) {
                        return;
                    }
                    panel.pending_problem_key = None;
                    match result {
                        Ok(mut problems) => {
                            let source_is_current = panel
                                .active_buffer
                                .as_ref()
                                .filter(|buffer| buffer.entity_id() == requested_buffer.entity_id())
                                .is_some_and(|buffer| {
                                    let source = buffer.read(cx).snapshot().text();
                                    ownership_problems_match_source(&problems, &source)
                                });
                            if source_is_current {
                                problems.problems.sort_by(|left, right| {
                                    ownership_problem_sort_key(left)
                                        .cmp(&ownership_problem_sort_key(right))
                                });
                                let previous_problem = panel.selected_problem().cloned();
                                let previous_id = panel.selected_problem_id.clone();
                                panel.update_repair_verification(&problems, cx);
                                panel.problem_status = ownership_problem_status(&problems).into();
                                panel.problems = problems;
                                panel.last_problem_key = Some(request_key.clone());
                                let selected = previous_id
                                    .as_deref()
                                    .and_then(|id| {
                                        panel
                                            .problems
                                            .problems
                                            .iter()
                                            .position(|problem| problem.id == id)
                                    })
                                    .or_else(|| {
                                        previous_problem.as_ref().and_then(|previous| {
                                            reconciled_ownership_problem_index(
                                                previous,
                                                &panel.problems.problems,
                                            )
                                        })
                                    })
                                    .or_else(|| panel.problem_index_at_cursor(cx))
                                    .or_else(|| (!panel.problems.problems.is_empty()).then_some(0));
                                let selected_id =
                                    selected.map(|index| panel.problems.problems[index].id.clone());
                                if selected_id != previous_id {
                                    panel.selection_epoch = panel.selection_epoch.wrapping_add(1);
                                    panel.model = OwnershipModel::default();
                                    panel.last_model_key = None;
                                    panel.pending_model_key = None;
                                }
                                panel.selected_problem_id = selected_id;
                                panel.apply_display_to_editor(cx);
                                if panel.active {
                                    panel.schedule_refresh(cx);
                                }
                            } else {
                                panel.last_problem_key = None;
                                panel.problem_status =
                                    "Source changed while ownership facts were loading.".into();
                            }
                        }
                        Err(error) => {
                            panel.last_problem_key = None;
                            panel.problem_status =
                                format!("Could not check Rust learning problems: {error}").into();
                            if let Some(verification) = &mut panel.repair_verification
                                && matches!(verification.state, RepairVerificationState::Checking)
                            {
                                verification.state = RepairVerificationState::Failed(
                                    format!("Cargo check could not verify this repair: {error}")
                                        .into(),
                                );
                            }
                        }
                    }
                    cx.notify();
                })
                .ok();
        });
    }

    fn cue_range(&mut self, range: lsp::Range, cx: &mut Context<Self>) {
        let (Some(buffer), Some(editor)) = (
            self.active_buffer.clone(),
            self.active_editor.as_ref().and_then(WeakEntity::upgrade),
        ) else {
            return;
        };
        let text_range = {
            let snapshot = buffer.read(cx).snapshot();
            let start = snapshot.clip_point_utf16(point_from_lsp(range.start), Bias::Left);
            let end = snapshot.clip_point_utf16(point_from_lsp(range.end), Bias::Right);
            snapshot.anchor_after(snapshot.point_utf16_to_point(start))
                ..snapshot.anchor_before(snapshot.point_utf16_to_point(end))
        };
        let multi_range = {
            let multibuffer = editor.read(cx).buffer().clone();
            let snapshot = multibuffer.read(cx).snapshot(cx);
            snapshot.buffer_anchor_range_to_anchor_range(text_range)
        };
        let Some(multi_range) = multi_range else {
            return;
        };
        editor.update(cx, |editor, cx| {
            editor.clear_row_highlights::<OwnershipStudioCue>();
            editor.highlight_background(
                HighlightKey::RustOwnershipWorkbench,
                std::slice::from_ref(&multi_range),
                |_, theme| theme.colors().editor_document_highlight_write_background,
                cx,
            );
            editor.highlight_rows::<OwnershipStudioCue>(
                multi_range,
                |cx| cx.theme().colors().editor_highlighted_line_background,
                RowHighlightOptions {
                    autoscroll: true,
                    include_gutter: true,
                },
                cx,
            );
        });
    }

    fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        if !self.active {
            return;
        }
        self.refresh_task = cx.spawn(async move |panel, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            panel.update(cx, |panel, cx| panel.refresh(cx)).ok();
        });
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.selected_problem_id.is_none()
            && (self.pending_problem_key.is_some() || !self.problems.problems.is_empty())
        {
            self.status_message =
                "Waiting for the selected compiler diagnostic before loading its guide…".into();
            cx.notify();
            return;
        }
        let Some(editor) = self.active_editor.as_ref().and_then(WeakEntity::upgrade) else {
            self.status_message = "Open a Rust source file to inspect ownership.".into();
            self.model = OwnershipModel::default();
            cx.notify();
            return;
        };
        let problem_position = self
            .selected_problem()
            .map(|problem| problem.model_position);
        let Some((buffer, position)) = editor.read_with(cx, |editor, cx| {
            let multibuffer = editor.buffer().read(cx);
            let snapshot = multibuffer.snapshot(cx);
            let (anchor, buffer_snapshot) =
                snapshot.anchor_to_buffer_anchor(editor.selections.newest_anchor().head())?;
            let buffer = multibuffer.buffer(anchor.buffer_id)?;
            let cursor_position = anchor.to_point_utf16(buffer_snapshot);
            let position = problem_position
                .map(|position| {
                    buffer_snapshot.clip_point_utf16(point_from_lsp(position), Bias::Left)
                })
                .unwrap_or(cursor_position);
            Some((buffer, position))
        }) else {
            return;
        };

        self.active_buffer = Some(buffer.clone());
        self.active_position = Some(position);
        let source = buffer.read(cx).snapshot().text();
        let request_key = ModelRequestKey {
            source_hash: ownership_source_hash(&source),
            problem_id: self.selected_problem_id.clone(),
            position,
            selection_epoch: self.selection_epoch,
        };
        if self.pending_model_key.as_ref() == Some(&request_key)
            || self.last_model_key.as_ref() == Some(&request_key)
                && ownership_model_matches_source(&self.model, &source)
        {
            return;
        }
        self.pending_model_key = Some(request_key.clone());
        self.clear_editor_cue(cx);
        self.status_message = "Checking compiler-backed learning context…".into();
        cx.notify();
        let requested_buffer = buffer.clone();
        let task = rust_analyzer_ext::ownership_model(self.project.clone(), buffer, position, cx);
        self.refresh_task = cx.spawn(async move |panel, cx| {
            let model = task.await;
            panel
                .update(cx, |panel, cx| {
                    // `Task` cancellation is best-effort. A completed LSP response can still be
                    // delivered after a newer diagnostic was selected, so validate its full
                    // request identity before touching any visible state.
                    if !model_response_is_current(
                        panel.pending_model_key.as_ref(),
                        &request_key,
                        panel.selected_problem_id.as_deref(),
                        panel.selection_epoch,
                    ) {
                        return;
                    }
                    panel.pending_model_key = None;
                    match model {
                        Ok(model) => {
                            let source_is_current = panel
                                .active_buffer
                                .as_ref()
                                .filter(|buffer| buffer.entity_id() == requested_buffer.entity_id())
                                .is_some_and(|buffer| {
                                    let source = buffer.read(cx).snapshot().text();
                                    ownership_model_matches_source(&model, &source)
                                });
                            let problem_is_current = ownership_model_matches_problem(
                                &model,
                                request_key.problem_id.as_deref(),
                            );
                            if source_is_current && problem_is_current {
                                panel.status_message = ownership_status(&model).into();
                                panel.model = model;
                                panel.last_model_key = Some(request_key.clone());
                                panel.apply_display_to_editor(cx);
                                if let Some(range) = panel
                                    .selected_problem()
                                    .map(|problem| problem.primary_range)
                                {
                                    panel.cue_range(range, cx);
                                }
                            } else {
                                panel.last_model_key = None;
                                panel.status_message = if source_is_current {
                                    "Ignored compiler facts for a different issue; refreshing the locked diagnostic…"
                                        .into()
                                } else {
                                    "Source changed while ownership facts were loading; refreshing…"
                                        .into()
                                };
                                panel.schedule_refresh(cx);
                            }
                        }
                        Err(error) => {
                            panel.last_model_key = None;
                            panel.status_message =
                                format!("Ownership analysis failed: {error}").into();
                            panel.model = OwnershipModel::default();
                        }
                    }
                    cx.notify();
                })
                .ok();
        });
    }

    fn apply_repair(
        &mut self,
        repair_id: String,
        title: String,
        source_hash: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .model
            .repairs
            .iter()
            .any(|repair| repair.id == repair_id && repair_is_compiler_validated(repair))
        {
            self.status_message =
                "This is still a design candidate. Preview it and wait for rustc validation before applying."
                    .into();
            cx.notify();
            return;
        }
        let (Some(buffer), Some(position)) = (self.active_buffer.clone(), self.active_position)
        else {
            return;
        };
        self.repair_verification = self.selected_problem().map(|problem| RepairVerification {
            diagnostic_code: problem.diagnostic_code.clone(),
            category: problem.category.clone(),
            binding_name: problem.binding_name.clone(),
            original_line: problem.primary_range.start.line,
            repair_title: title.clone(),
            baseline_problem_signatures: self
                .problems
                .problems
                .iter()
                .map(ownership_problem_signature)
                .collect(),
            state: RepairVerificationState::Applying,
        });
        let live_source = buffer.read(cx).snapshot().text();
        if ownership_source_hash(&live_source) != source_hash {
            self.status_message =
                "The source changed since this preview was generated. Refreshing alternatives…"
                    .into();
            if let Some(verification) = &mut self.repair_verification {
                verification.state = RepairVerificationState::Failed(
                    "The preview was stale, so no edit was applied.".into(),
                );
            }
            self.schedule_refresh(cx);
            cx.notify();
            return;
        }
        let action = rust_analyzer_ext::ownership_repair(
            self.project.clone(),
            buffer.clone(),
            position,
            repair_id,
            source_hash,
            cx,
        );
        let project = self.project.clone();
        self.status_message = format!("Applying {title}…").into();
        self.refresh_task = cx.spawn(async move |panel, cx| {
            let result = async {
                let action = action
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("repair is stale or no longer available"))?;
                project
                    .update(cx, |project, cx| {
                        project.apply_code_action(buffer, action, true, cx)
                    })
                    .await?;
                anyhow::Ok(())
            }
            .await;

            panel
                .update(cx, |panel, cx| {
                    match result {
                        Ok(()) => {
                            panel.status_message = "Repair applied. Use Undo to revert it.".into();
                            if let Some(verification) = &mut panel.repair_verification {
                                verification.state = RepairVerificationState::Checking;
                            }
                            panel.schedule_problem_scan(cx);
                        }
                        Err(error) => {
                            panel.status_message =
                                format!("Could not apply repair: {error}").into();
                            if let Some(verification) = &mut panel.repair_verification {
                                verification.state = RepairVerificationState::Failed(
                                    format!("The editor could not apply the rewrite: {error}")
                                        .into(),
                                );
                            }
                        }
                    }
                    cx.notify();
                })
                .ok();
        });
    }
}

fn repair_verification_outcome(
    verification: &RepairVerification,
    problems: &OwnershipProblems,
) -> RepairVerificationState {
    let matching = problems
        .problems
        .iter()
        .filter(|problem| {
            problem.category == verification.category
                && problem.binding_name == verification.binding_name
                && problem.diagnostic_code == verification.diagnostic_code
        })
        .min_by_key(|problem| {
            problem
                .primary_range
                .start
                .line
                .abs_diff(verification.original_line)
        });
    if let Some(problem) = matching
        && problem
            .primary_range
            .start
            .line
            .abs_diff(verification.original_line)
            <= 8
    {
        RepairVerificationState::StillPresent {
            current_line: problem.primary_range.start.line,
        }
    } else {
        let summaries = problems
            .problems
            .iter()
            .filter(|problem| {
                !verification
                    .baseline_problem_signatures
                    .contains(&ownership_problem_signature(problem))
            })
            .take(5)
            .map(|problem| {
                format!(
                    "{} on `{}` at line {}",
                    problem.diagnostic_code.as_deref().unwrap_or("rustc"),
                    problem.binding_name,
                    problem.primary_range.start.line + 1
                )
            })
            .collect::<Vec<_>>();
        if summaries.is_empty() {
            RepairVerificationState::Resolved {
                remaining_file_problems: problems.problems.len(),
            }
        } else {
            RepairVerificationState::IntroducedProblems { summaries }
        }
    }
}

fn ownership_problem_signature(problem: &OwnershipProblem) -> String {
    format!(
        "{}|{}|{}",
        problem.diagnostic_code.as_deref().unwrap_or("rustc"),
        problem.category,
        problem.binding_name
    )
}

fn ownership_model_matches_source(model: &OwnershipModel, source: &str) -> bool {
    model.status == "rust_analyzer_unavailable"
        || model.source_hash == ownership_source_hash(source)
}

async fn generate_c_artifact(
    source_path: PathBuf,
    source_hash: String,
) -> anyhow::Result<GeneratedCArtifact> {
    let manifest_path = source_path
        .ancestors()
        .map(|ancestor| ancestor.join("Cargo.toml"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            anyhow::anyhow!("No Cargo.toml was found above {}.", source_path.display())
        })?;
    let target_dir = paths::data_dir()
        .join("rust-workbench")
        .join("generated-c")
        .join(&source_hash);
    std::fs::create_dir_all(&target_dir)?;

    let (cargo, backend) = if let Some(custom_cargo) =
        std::env::var_os("RUST_WORKBENCH_RUSTIC_CARGO").map(PathBuf::from)
    {
        if !custom_cargo.is_file() {
            anyhow::bail!(
                "RUST_WORKBENCH_RUSTIC_CARGO points to a missing file: {}",
                custom_cargo.display()
            );
        }
        (custom_cargo, "custom rustc_codegen_c toolchain".to_owned())
    } else {
        let mut locate = async_process::Command::new("rustup");
        locate
            .args(["which", "--toolchain", "rustic", "cargo"])
            .kill_on_drop(true);
        let located = locate.output().await?;
        if !located.status.success() {
            anyhow::bail!(
                "The verified `rustic` toolchain is not installed. Install a pinned rustc_codegen_c release, or set RUST_WORKBENCH_RUSTIC_CARGO to its cargo executable. Conceptual C remains available."
            );
        }
        let cargo = PathBuf::from(String::from_utf8_lossy(&located.stdout).trim());
        (
            cargo,
            "rustc_codegen_c via rustup toolchain `rustic`".to_owned(),
        )
    };

    let mut command = async_process::Command::new(&cargo);
    command
        .args([
            "build",
            "-Z",
            "build-std",
            "--manifest-path",
            manifest_path.to_string_lossy().as_ref(),
        ])
        .env("RUSTUP_TOOLCHAIN", "rustic")
        .env("RUSTC_BOOTSTRAP", "1")
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("CARGO_PROFILE_DEV_OPT_LEVEL", "0")
        .env("CARGO_PROFILE_DEV_CODEGEN_UNITS", "1")
        .env("CARGO_PROFILE_DEV_DEBUG", "1")
        .kill_on_drop(true);
    let output = command.output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let diagnostic = stderr
            .chars()
            .rev()
            .take(12_000)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        anyhow::bail!(
            "Generated C is blocked because the experimental backend could not build this target:\n{diagnostic}"
        );
    }
    let c_path = newest_generated_c_file(&target_dir).ok_or_else(|| {
        anyhow::anyhow!(
            "The backend completed but produced no .c artifact under {}.",
            target_dir.display()
        )
    })?;
    let code = std::fs::read_to_string(&c_path)?;
    let code = generated_c_preview(code, &c_path, 400_000);
    Ok(GeneratedCArtifact {
        code: code.into(),
        path: c_path,
        source_hash,
        backend: backend.into(),
    })
}

fn generated_c_preview(code: String, c_path: &std::path::Path, byte_limit: usize) -> String {
    if code.len() <= byte_limit {
        return code;
    }

    let preview_end = code
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= byte_limit)
        .last()
        .unwrap_or(0);
    format!(
        "{}\n\n/* Rust Workbench truncated this preview. Open the full artifact at {}. */",
        &code[..preview_end],
        c_path.display()
    )
}

fn newest_generated_c_file(root: &std::path::Path) -> Option<PathBuf> {
    let mut directories = vec![root.to_path_buf()];
    let mut candidates = Vec::new();
    while let Some(directory) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                directories.push(path);
            } else if path.extension().is_some_and(|extension| extension == "c") {
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok();
                candidates.push((modified, path));
            }
        }
    }
    candidates.sort_by_key(|(modified, _)| *modified);
    candidates.pop().map(|(_, path)| path)
}

fn ownership_problems_match_source(problems: &OwnershipProblems, source: &str) -> bool {
    problems.status == "rust_analyzer_unavailable"
        || problems.source_hash == ownership_source_hash(source)
}

fn ownership_model_matches_problem(
    model: &OwnershipModel,
    requested_problem_id: Option<&str>,
) -> bool {
    requested_problem_id.is_none()
        || model.schema_version < 11
        || model.selected_problem_id.as_deref() == requested_problem_id
}

fn ownership_problem_sort_key(problem: &OwnershipProblem) -> (u32, u32, &str) {
    (
        problem.primary_range.start.line,
        problem.primary_range.start.character,
        problem.id.as_str(),
    )
}

fn position_is_before(left: lsp::Position, right: lsp::Position) -> bool {
    (left.line, left.character) < (right.line, right.character)
}

fn position_is_in_range(position: lsp::Position, range: lsp::Range) -> bool {
    !position_is_before(position, range.start) && !position_is_before(range.end, position)
}

fn ownership_problem_index_at_position(
    problems: &[OwnershipProblem],
    position: lsp::Position,
) -> Option<usize> {
    problems
        .iter()
        .enumerate()
        .filter(|(_, problem)| position_is_in_range(position, problem.primary_range))
        .min_by_key(|(index, problem)| {
            (
                problem
                    .primary_range
                    .end
                    .line
                    .saturating_sub(problem.primary_range.start.line),
                problem
                    .primary_range
                    .end
                    .character
                    .saturating_sub(problem.primary_range.start.character),
                *index,
            )
        })
        .map(|(index, _)| index)
}

fn reconciled_ownership_problem_index(
    previous: &OwnershipProblem,
    current: &[OwnershipProblem],
) -> Option<usize> {
    current
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.diagnostic_code == previous.diagnostic_code
                && candidate.category == previous.category
                && candidate.binding_name == previous.binding_name
        })
        .min_by_key(|(index, candidate)| {
            (
                candidate
                    .primary_range
                    .start
                    .line
                    .abs_diff(previous.primary_range.start.line),
                candidate
                    .primary_range
                    .start
                    .character
                    .abs_diff(previous.primary_range.start.character),
                *index,
            )
        })
        .map(|(index, _)| index)
}

fn distance_to_range(cursor: lsp::Position, range: lsp::Range) -> (u32, u32) {
    if position_is_before(cursor, range.start) {
        let line_distance = range.start.line - cursor.line;
        let character_distance = if line_distance == 0 {
            range.start.character - cursor.character
        } else {
            range.start.character
        };
        (line_distance, character_distance)
    } else if position_is_before(range.end, cursor) {
        let line_distance = cursor.line - range.end.line;
        let character_distance = if line_distance == 0 {
            cursor.character - range.end.character
        } else {
            cursor.character
        };
        (line_distance, character_distance)
    } else {
        (0, 0)
    }
}

fn problem_distance(cursor: lsp::Position, problem: &OwnershipProblem) -> (u32, u32) {
    std::iter::once(problem.primary_range)
        .chain(std::iter::once(problem.binding_range))
        .chain(problem.related_ranges.iter().copied())
        .map(|range| distance_to_range(cursor, range))
        .min()
        .unwrap_or((u32::MAX, u32::MAX))
}

fn nearest_ownership_problem_index(
    problems: &[OwnershipProblem],
    cursor: lsp::Position,
) -> Option<usize> {
    problems
        .iter()
        .enumerate()
        .min_by_key(|(index, problem)| (problem_distance(cursor, problem), *index))
        .map(|(index, _)| index)
}

fn relative_problem_index(
    selected: Option<usize>,
    problem_count: usize,
    direction: isize,
) -> Option<usize> {
    if problem_count == 0 {
        return None;
    }
    let selected = selected.unwrap_or(0) as isize;
    Some((selected + direction).rem_euclid(problem_count as isize) as usize)
}

fn adjusted_panel_font_scale(current: u16, delta_percent: i16) -> u16 {
    (current as i32 + i32::from(delta_percent)).clamp(
        i32::from(MIN_PANEL_FONT_SCALE_PERCENT),
        i32::from(MAX_PANEL_FONT_SCALE_PERCENT),
    ) as u16
}

fn ownership_problem_status(problems: &OwnershipProblems) -> String {
    match problems.status.as_str() {
        "rust_analyzer_unavailable" => {
            "The patched rust-analyzer is not running for this buffer.".to_owned()
        }
        "waiting_for_compiler" => "Waiting for Cargo check learning facts…".to_owned(),
        _ if problems.problems.is_empty() => {
            "No supported Rust diagnostics in this file. Select a value to inspect valid ownership flow."
                .to_owned()
        }
        _ if problems.problems.len() == 1 => "One explainable Rust problem found.".to_owned(),
        _ => format!(
            "{} explainable Rust problems found.",
            problems.problems.len()
        ),
    }
}

fn ownership_source_hash(source: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn repair_is_compiler_validated(repair: &rust_analyzer_ext::OwnershipRepair) -> bool {
    repair.compiler_validated || repair.validation_state == "validated"
}

fn ownership_status(model: &OwnershipModel) -> String {
    if model.status == "rust_analyzer_unavailable" {
        return "The patched rust-analyzer is not running for this buffer.".to_owned();
    }
    if model.events.is_empty() {
        if model.source_context.is_some() || !model.operations.is_empty() {
            return "Compiler diagnostic context is ready; this error family has no ownership-event timeline."
                .to_owned();
        }
        return "Waiting for compiler facts. Save the file or run Cargo check.".to_owned();
    }
    let precision = if model.precision == "compiler_exact" {
        "Compiler exact"
    } else {
        "Estimated"
    };
    let bounded = if model.truncated {
        " · large result bounded for responsiveness"
    } else {
        ""
    };
    match &model.selected_place {
        Some(place) => format!("{precision} ownership timeline for {place}{bounded}"),
        None => format!("{precision} ownership timeline{bounded}"),
    }
}

impl Focusable for RustWorkbenchPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for RustWorkbenchPanel {}

impl Panel for RustWorkbenchPanel {
    fn persistent_name() -> &'static str {
        "Rust Learning Debugger"
    }

    fn panel_key() -> &'static str {
        PANEL_KEY
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        DockPosition::Right
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        _position: DockPosition,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        px(560.)
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::Code)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Rust Learning Debugger")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(Toggle)
    }

    fn starts_open(&self, _window: &Window, _cx: &App) -> bool {
        false
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        // Dock activation runs inside a Workspace update. Reading `self.workspace` synchronously
        // here would attempt to lease that same entity twice and panic in GPUI. Defer all
        // cross-entity work until the current effect cycle has released the Workspace lease.
        cx.defer_in(window, |panel, _window, cx| {
            if panel.active {
                panel.observe_active_editor(cx);
                if panel.problems.problems.is_empty() {
                    if panel.pending_problem_key.is_none() {
                        panel.schedule_problem_scan(cx);
                    }
                } else {
                    panel.schedule_cursor_problem_selection(cx);
                }
            } else {
                panel.cancel_generated_c(cx);
                panel.clear_editor_cue(cx);
            }
        });
    }

    fn activation_priority(&self) -> u32 {
        7
    }
}

impl Render for RustWorkbenchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed_sections = self.collapsed_sections.clone();
        let expanded_operations = self.expanded_operations.clone();
        let repair_verification = self.repair_verification.clone();
        let c_view_mode = self.c_view_mode;
        let c_generation_state = self.c_generation_state.clone();
        let generated_c = self.generated_c.clone();
        let exact_mode = self.exact_mode;
        let visual_step = self.visual_step;
        let preview_repair_id = self.preview_repair_id.clone();
        let show_repair_alternatives = self.show_repair_alternatives;
        let problem = self.selected_problem().cloned();
        let problem_count = self.problems.problems.len();
        let issue_list = self.problems.problems.clone();
        let selected_problem_index = self.selected_problem_index().unwrap_or(0);
        let selected_problem_label = problem.as_ref().map(|problem| {
            let target = resolved_problem_target(problem, &self.model);
            if let Some(code) = problem.diagnostic_code.as_deref() {
                format!("{code} · `{target}`")
            } else {
                format!("`{target}`")
            }
        });
        let font_scale_percent = self.font_scale_percent;
        let display_preferences = self.display_preferences.clone();
        let show_display_controls = self.show_display_controls;
        let show_issue_list = self.show_issue_list;
        let panel_rem_size = window.rem_size() * (f32::from(font_scale_percent) / 100.0);
        let problem_status = self.problem_status.clone();
        let status_message = self.status_message.clone();
        WithRemSize::new(panel_rem_size).size_full().child(
            v_flex()
                .id("rust-ownership-workbench")
                .track_focus(&self.focus_handle)
                .size_full()
                .bg(cx.theme().colors().panel_background)
                .border_t_2()
                .border_color(cx.theme().status().error)
                .child(
                    h_flex()
                        .p_3()
                        .gap_2()
                        .justify_between()
                        .border_b_1()
                        .border_color(cx.theme().colors().border)
                        .child(
                            v_flex()
                                .gap_1()
                                .child(Label::new("Rust Ownership Guide").size(LabelSize::Large))
                                .child(
                                    Label::new(
                                        "One problem → one value trace → one rule → verified fixes",
                                    )
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new(
                                        "rust-learning-display",
                                        format!(
                                            "Hints: {}",
                                            display_profile_label(display_preferences.profile)
                                        ),
                                    )
                                    .on_click(cx.listener(
                                        |panel, _, _window, cx| {
                                            panel.show_display_controls =
                                                !panel.show_display_controls;
                                            cx.notify();
                                        },
                                    )),
                                )
                                .child(Button::new("decrease-coach-font", "A−").on_click(
                                    cx.listener(|panel, _, _window, cx| {
                                        panel.adjust_font_scale(-PANEL_FONT_SCALE_STEP_PERCENT, cx);
                                    }),
                                ))
                                .child(Button::new("increase-coach-font", "A+").on_click(
                                    cx.listener(|panel, _, _window, cx| {
                                        panel.adjust_font_scale(PANEL_FONT_SCALE_STEP_PERCENT, cx);
                                    }),
                                ))
                                .child(Button::new("refresh-ownership", "Refresh").on_click(
                                    cx.listener(|panel, _, _window, cx| {
                                        panel.last_model_key = None;
                                        panel.pending_model_key = None;
                                        panel.last_problem_key = None;
                                        panel.pending_problem_key = None;
                                        panel.schedule_problem_scan(cx);
                                    }),
                                )),
                        ),
                )
                .when(show_display_controls, |this| {
                    this.child(render_display_controls(&display_preferences, cx))
                })
                .when(problem_count > 0, |this| {
                    this.child(
                        h_flex()
                            .mx_3()
                            .mt_2()
                            .p_2()
                            .gap_2()
                            .justify_between()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().colors().border_variant)
                            .child(
                                Button::new("previous-ownership-issue", "← Previous").on_click(
                                    cx.listener(|panel, _, _window, cx| {
                                        panel.select_relative_problem(-1, cx);
                                    }),
                                ),
                            )
                            .child(
                                v_flex()
                                    .items_center()
                                    .child(
                                        Label::new(format!(
                                            "Issue {} of {problem_count}",
                                            selected_problem_index + 1
                                        ))
                                        .size(LabelSize::Small),
                                    )
                                    .when_some(selected_problem_label, |this, label| {
                                        this.child(
                                            Label::new(label)
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted),
                                        )
                                    }),
                            )
                            .child(Button::new("next-ownership-issue", "Next →").on_click(
                                cx.listener(|panel, _, _window, cx| {
                                    panel.select_relative_problem(1, cx);
                                }),
                            ))
                            .child(
                                Button::new(
                                    "toggle-ownership-issue-list",
                                    if show_issue_list {
                                        "Hide issues"
                                    } else {
                                        "All issues"
                                    },
                                )
                                .on_click(cx.listener(
                                    |panel, _, _window, cx| {
                                        panel.show_issue_list = !panel.show_issue_list;
                                        cx.notify();
                                    },
                                )),
                            ),
                    )
                })
                .when(show_issue_list && problem_count > 0, |this| {
                    this.child(
                        v_flex()
                            .mx_3()
                            .mt_2()
                            .p_2()
                            .gap_1()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().colors().border_variant)
                            .child(
                                Label::new("Compiler issues in this file").size(LabelSize::Small),
                            )
                            .children(issue_list.into_iter().enumerate().map(|(index, issue)| {
                                let selected = index == selected_problem_index;
                                let code = issue
                                    .diagnostic_code
                                    .as_deref()
                                    .unwrap_or("rustc")
                                    .to_owned();
                                Button::new(
                                    SharedString::from(format!("ownership-issue-{index}")),
                                    format!(
                                        "{} {code} · line {} · `{}`",
                                        if selected { "●" } else { "○" },
                                        issue.primary_range.start.line + 1,
                                        issue.binding_name
                                    ),
                                )
                                .on_click(cx.listener(
                                    move |panel, _, _window, cx| {
                                        panel.select_problem_index(index, true, cx);
                                    },
                                ))
                            })),
                    )
                })
                .child(
                    v_flex()
                        .mx_3()
                        .my_2()
                        .gap_1()
                        .child(
                            Label::new(problem_status)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .child(
                            Label::new(status_message)
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        ),
                )
                .child(
                    v_flex()
                        .id("rust-ownership-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_3()
                        .pb_4()
                        .gap_3()
                        .child(
                            v_flex()
                                .gap_3()
                                .child(render_beginner_flow(
                                    problem.as_ref(),
                                    &self.problems,
                                    &self.model,
                                    visual_step,
                                    repair_verification.as_ref(),
                                    preview_repair_id.as_deref(),
                                    show_repair_alternatives,
                                    exact_mode,
                                    cx,
                                ))
                                .child(render_learning_section(
                                    LearningSection::Advanced,
                                    collapsed_sections.contains(&LearningSection::Advanced),
                                    |cx| {
                                        v_flex()
                                            .gap_3()
                                            .child(render_codebase_context(&self.model, cx))
                                            .child(render_operation_insights(
                                                &self.model,
                                                &expanded_operations,
                                                cx,
                                            ))
                                            .child(render_timeline(&self.model, exact_mode, cx))
                                            .child(render_lifetimes(&self.model, exact_mode, cx))
                                            .child(render_memory(&self.model, exact_mode, cx))
                                            .child(render_c_view(
                                                &self.model,
                                                exact_mode,
                                                c_view_mode,
                                                c_generation_state,
                                                generated_c,
                                                cx,
                                            ))
                                            .into_any_element()
                                    },
                                    cx,
                                ))
                                .child(
                                    Button::new(
                                        "toggle-exact-details",
                                        if exact_mode {
                                            "Hide MIR coordinates"
                                        } else {
                                            "Show MIR coordinates"
                                        },
                                    )
                                    .on_click(cx.listener(
                                        |panel, _, _window, cx| {
                                            panel.exact_mode = !panel.exact_mode;
                                            cx.notify();
                                        },
                                    )),
                                )
                                .into_any_element(),
                        ),
                ),
        )
    }
}

#[derive(Clone)]
struct VisualMoment {
    phase: String,
    title: String,
    explanation: String,
    range: lsp::Range,
    state: String,
}

fn visual_moments(problem: Option<&OwnershipProblem>, model: &OwnershipModel) -> Vec<VisualMoment> {
    if let Some(graph) = &model.conflict_graph
        && !graph.snapshots.is_empty()
    {
        return graph
            .snapshots
            .iter()
            .map(|snapshot| VisualMoment {
                phase: snapshot.phase.clone(),
                title: snapshot.title.clone(),
                explanation: snapshot.explanation.clone(),
                range: snapshot.range,
                state: if snapshot.phase == "operation_rejected" {
                    "rejected".to_owned()
                } else if snapshot.phase == "borrow_ended" {
                    "available_afterward".to_owned()
                } else {
                    "borrow_live".to_owned()
                },
            })
            .collect();
    }

    let mut moments = model
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.kind.as_str(),
                "move"
                    | "partial_move"
                    | "borrow_shared"
                    | "borrow_mutable"
                    | "borrow_activate"
                    | "invalid_use"
                    | "borrow_end"
                    | "reinitialize"
                    | "drop"
            )
        })
        .take(12)
        .map(|event| VisualMoment {
            phase: event.kind.clone(),
            title: format!("{} `{}`", visual_event_title(&event.kind), event.place),
            explanation: guided_event_explanation(&event.kind, &event.place),
            range: event.range,
            state: event.state.clone(),
        })
        .collect::<Vec<_>>();
    if moments.is_empty()
        && let Some(problem) = problem
    {
        let (title, what, why) = problem_story(&problem.category, &problem.binding_name);
        moments = vec![
            VisualMoment {
                phase: "contract".to_owned(),
                title: "1 · Establish the expected contract".to_owned(),
                explanation: what,
                range: problem.binding_range,
                state: "available".to_owned(),
            },
            VisualMoment {
                phase: "operation_rejected".to_owned(),
                title,
                explanation: if problem.message.is_empty() {
                    why
                } else {
                    format!("{}\n\nWhy: {why}", problem.message)
                },
                range: problem.primary_range,
                state: "rejected".to_owned(),
            },
            VisualMoment {
                phase: "repair".to_owned(),
                title: "3 · Choose a repair that matches intent".to_owned(),
                explanation: "Compare the repair choices below. A source rewrite is only marked successful after the compiler confirms this diagnostic is gone."
                    .to_owned(),
                range: problem.primary_range,
                state: "decision".to_owned(),
            },
        ];
    }
    moments
}

fn visual_event_title(kind: &str) -> &'static str {
    match kind {
        "move" => "Move",
        "partial_move" => "Partial move",
        "borrow_shared" => "Shared borrow",
        "borrow_mutable" => "Mutable borrow",
        "borrow_activate" => "Borrow activates",
        "invalid_use" => "Rejected use",
        "borrow_end" => "Borrow ends",
        "reinitialize" => "Reinitialized",
        "drop" => "Dropped",
        _ => "State change",
    }
}

fn render_beginner_flow(
    problem: Option<&OwnershipProblem>,
    problems: &OwnershipProblems,
    model: &OwnershipModel,
    selected_step: usize,
    verification: Option<&RepairVerification>,
    preview_repair_id: Option<&str>,
    show_repair_alternatives: bool,
    exact_mode: bool,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    v_flex()
        .gap_3()
        .child(render_visual_problem_header(problem, model, cx))
        .child(render_beginner_concept(problem, cx))
        .child(render_guided_visual_step(problem, model, selected_step, cx))
        .child(render_guided_fix_step(
            problem,
            problems,
            model,
            verification,
            preview_repair_id,
            show_repair_alternatives,
            exact_mode,
            cx,
        ))
        .into_any_element()
}

fn render_guided_visual_step(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
    selected_step: usize,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    v_flex()
        .p_3()
        .gap_3()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new("3 · See the access and memory").size(LabelSize::Large))
        .child(
            Label::new(
                "The first picture follows the rejected operation. The second separates references, owners, stack handles, and heap storage.",
            )
            .size(LabelSize::Small)
            .color(Color::Muted),
        )
        .child(render_value_journey(problem, model, selected_step, cx))
        .child(render_visual_memory_map(model, selected_step, cx))
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_guided_fix_step(
    problem: Option<&OwnershipProblem>,
    problems: &OwnershipProblems,
    model: &OwnershipModel,
    verification: Option<&RepairVerification>,
    preview_repair_id: Option<&str>,
    show_repair_alternatives: bool,
    exact_mode: bool,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new("4 · Choose and verify a fix").size(LabelSize::Large))
        .child(render_guided_repairs(
            problem,
            model,
            preview_repair_id,
            show_repair_alternatives,
            exact_mode,
            cx,
        ))
        .when(
            verification.is_some() || (problems.problems.is_empty() && problems.status == "ready"),
            |this| {
                this.child(render_coach_result(
                    problem,
                    problems,
                    model,
                    verification,
                    cx,
                ))
            },
        )
        .into_any_element()
}

fn render_value_journey(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
    selected_step: usize,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    if let Some(graph) = &model.conflict_graph
        && !graph.nodes.is_empty()
        && !graph.snapshots.is_empty()
    {
        return render_guided_conflict_graph(graph, selected_step, cx);
    }
    if problem.is_some_and(|problem| problem.category == "immutable_mutation")
        && (model.mutation_requirement.is_some() || !model.operations.is_empty())
    {
        return render_guided_mutation_requirement(problem, model, cx);
    }
    if model.value_trace.is_empty() {
        let moments = visual_moments(problem, model);
        let selected_step = selected_step.min(moments.len().saturating_sub(1));
        return render_visual_timeline(
            &moments,
            selected_step,
            moments.get(selected_step).cloned(),
            cx,
        );
    }

    let visible = model.value_trace.iter().take(8).collect::<Vec<_>>();
    let selected_index = selected_step.min(visible.len().saturating_sub(1));
    let selected = visible.get(selected_index).copied().cloned();
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new("Follow the value").size(LabelSize::Large))
        .child(
            Label::new(
                "Choose a step. The guide shows only that moment and highlights its source line.",
            )
            .size(LabelSize::Small)
            .color(Color::Muted),
        )
        .child(
            h_flex()
                .gap_1()
                .flex_wrap()
                .children(visible.into_iter().enumerate().map(|(index, step)| {
                    Button::new(
                        SharedString::from(format!("value-trace-{index}")),
                        if index == selected_index {
                            format!("● {} · {}", index + 1, trace_arrow_label(&step.kind))
                        } else {
                            format!("{} · {}", index + 1, trace_arrow_label(&step.kind))
                        },
                    )
                    .on_click(cx.listener(move |panel, _, _window, cx| {
                        panel.select_visual_step(index, cx);
                    }))
                })),
        )
        .when_some(selected, |this, step| {
            let destination = step
                .to_label
                .as_deref()
                .map(|destination| {
                    format!(
                        "`{}`  ──{}──►  `{destination}`",
                        step.from_label,
                        trace_arrow_label(&step.kind)
                    )
                })
                .unwrap_or_else(|| {
                    format!("`{}` · {}", step.from_label, trace_arrow_label(&step.kind))
                });
            this.child(
                v_flex()
                    .p_2()
                    .gap_1()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().status().info)
                    .child(Label::new(destination).size(LabelSize::Small))
                    .child(Label::new(step.explanation).size(LabelSize::Small))
                    .child(
                        Label::new(format!(
                            "Now: {} · Memory: {}",
                            step.source_state, step.allocation_effect
                        ))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                    ),
            )
        })
        .when(model.value_trace.len() > 8, |this| {
            this.child(
                Label::new(format!(
                    "{} later steps are available under Explore deeper.",
                    model.value_trace.len() - 8
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            )
        })
        .into_any_element()
}

fn render_guided_mutation_requirement(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let Some(problem) = problem else {
        return empty_card("Select the mutation error to inspect the required access.");
    };
    let Some(operation) = selected_mutation_operation(model) else {
        return empty_card(
            "The compiler reported a mutation error, but no responsible call signature was resolved.",
        );
    };
    let requirement = model.mutation_requirement.as_ref();
    let target_place = resolved_problem_target(problem, model);
    let access_source = requirement
        .map(|requirement| requirement.access_source.as_str())
        .unwrap_or("the current access path");
    let available_access = requirement
        .map(|requirement| readable_available_access(&requirement.available_access))
        .unwrap_or("insufficient mutable access");
    let required_access = requirement
        .map(|requirement| readable_access(&requirement.required_access))
        .unwrap_or_else(|| readable_access(&operation.required_access));
    let explanation = requirement
        .map(|requirement| requirement.explanation.as_str())
        .unwrap_or(operation.available_access.as_str());
    let access_context = model
        .source_context
        .as_ref()
        .and_then(|context| {
            context
                .breadcrumbs
                .iter()
                .find(|item| item.kind == "function")
        })
        .filter(|_| matches!(access_source, "&self" | "&mut self" | "self"))
        .map(|function| format!("{}({access_source})", function.label))
        .unwrap_or_else(|| access_source.to_owned());
    let range = operation.range;
    let alternatives = operation
        .alternatives
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>();
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(
            Label::new(format!(
                "Why Rust rejects {}() on `{target_place}`",
                operation.name
            ))
            .size(LabelSize::Large),
        )
        .child(
            Label::new(format!(
                "`{access_context}`  ── {available_access} ──►  `{target_place}`"
            ))
            .size(LabelSize::Small)
            .buffer_font(cx),
        )
        .child(
            Label::new(format!(
                "`{}`  ── requires {required_access} ──►  `{target_place}`",
                operation.signature
            ))
            .size(LabelSize::Small)
            .buffer_font(cx),
        )
        .child(
            h_flex()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().status().error)
                .child(
                    Label::new(format!(
                        "{available_access} cannot satisfy {required_access} → the call is rejected"
                    ))
                    .size(LabelSize::Small)
                    .color(Color::Error),
                ),
        )
        .child(Label::new(explanation).size(LabelSize::Small))
        .child(
            Label::new(operation.why_required.clone())
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .child(
            Button::new("guided-operation-source", "Show this call in code")
                .on_click(cx.listener(move |panel, _, _window, cx| panel.cue_range(range, cx))),
        )
        .when(!operation.effect_facts.is_empty(), |this| {
            this.child(Label::new("What the method changes").size(LabelSize::Small))
                .children(operation.effect_facts.iter().take(3).map(|effect| {
                    Label::new(format!("• {}", effect.summary))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted)
                }))
        })
        .when(!alternatives.is_empty(), |this| {
            this.child(
                Label::new("Related methods are not always equivalent").size(LabelSize::Small),
            )
            .children(alternatives.into_iter().map(|alternative| {
                Label::new(format!(
                    "• {} — {}",
                    alternative.signature, alternative.difference
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted)
            }))
        })
        .into_any_element()
}

fn resolved_problem_target<'a>(
    problem: &'a OwnershipProblem,
    model: &'a OwnershipModel,
) -> &'a str {
    if problem.category != "immutable_mutation" {
        return problem.binding_name.as_str();
    }
    if let Some(requirement) = model.mutation_requirement.as_ref()
        && !requirement.target_place.is_empty()
    {
        return requirement.target_place.as_str();
    }

    // Older/in-flight model responses may not yet contain `mutationRequirement`. The model's
    // selected place still usually carries the field or local rustc rejected. Prefer whichever
    // candidate is more specific so `self.events` can never collapse back to just `self`.
    let problem_target = problem.binding_name.as_str();
    let model_target = model.selected_place.as_deref().unwrap_or("");
    if mutation_target_specificity(model_target) > mutation_target_specificity(problem_target) {
        model_target
    } else {
        problem_target
    }
}

fn mutation_target_specificity(place: &str) -> (u8, usize) {
    let normalized = place.trim_matches(|character: char| {
        character.is_whitespace() || matches!(character, '`' | '&' | '*' | '(' | ')')
    });
    let detail = if normalized.is_empty() || matches!(normalized, "self" | "value" | "current") {
        0
    } else if normalized.contains('.') || normalized.contains('[') {
        2
    } else {
        1
    };
    (detail, normalized.len())
}

fn selected_mutation_operation(
    model: &OwnershipModel,
) -> Option<&rust_analyzer_ext::OwnershipOperationInsight> {
    model
        .mutation_requirement
        .as_ref()
        .and_then(|requirement| {
            model
                .operations
                .iter()
                .find(|operation| operation.id == requirement.operation_id)
        })
        .or_else(|| {
            model
                .operations
                .iter()
                .find(|operation| operation.required_access == "mutable_borrow")
        })
}

fn render_guided_conflict_graph(
    graph: &rust_analyzer_ext::OwnershipConflictGraph,
    selected_step: usize,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let snapshots = graph.snapshots.iter().take(6).cloned().collect::<Vec<_>>();
    let selected_index = selected_step.min(snapshots.len().saturating_sub(1));
    let selected = snapshots.get(selected_index).cloned();
    let node_label = |node_id: &str| {
        graph
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .map(|node| node.label.clone())
            .unwrap_or_else(|| node_id.to_owned())
    };
    let relationships = graph
        .edges
        .iter()
        .take(6)
        .map(|edge| {
            format!(
                "`{}`  ── {} ──►  `{}`",
                node_label(&edge.from),
                edge.label,
                node_label(&edge.to)
            )
        })
        .collect::<Vec<_>>();

    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new("Who borrows what").size(LabelSize::Large))
        .child(Label::new(graph.title.clone()).size(LabelSize::Small))
        .child(
            Label::new(graph.summary.clone())
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .children(relationships.into_iter().map(|relationship| {
            h_flex()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    Label::new(relationship)
                        .size(LabelSize::Small)
                        .buffer_font(cx),
                )
        }))
        .child(
            h_flex()
                .gap_1()
                .flex_wrap()
                .children(snapshots.iter().enumerate().map(|(index, snapshot)| {
                    Button::new(
                        SharedString::from(format!("borrow-snapshot-{index}")),
                        if index == selected_index {
                            format!("● {} · {}", index + 1, snapshot.title)
                        } else {
                            format!("{} · {}", index + 1, snapshot.title)
                        },
                    )
                    .on_click(cx.listener(move |panel, _, _window, cx| {
                        panel.select_visual_step(index, cx);
                    }))
                })),
        )
        .when_some(selected, |this, snapshot| {
            let color = if snapshot.phase == "operation_rejected" {
                Color::Error
            } else if snapshot.phase == "borrow_ended" {
                Color::Success
            } else {
                Color::Info
            };
            this.child(
                v_flex()
                    .p_2()
                    .gap_1()
                    .rounded_md()
                    .border_1()
                    .border_color(match color {
                        Color::Error => cx.theme().status().error,
                        Color::Success => cx.theme().status().success,
                        _ => cx.theme().status().info,
                    })
                    .child(
                        Label::new(snapshot.explanation)
                            .size(LabelSize::Small)
                            .color(color),
                    )
                    .children(snapshot.states.into_iter().map(|state| {
                        let label = node_label(&state.node_id);
                        v_flex()
                            .gap_0p5()
                            .child(
                                Label::new(format!("`{label}`: {}", state.state))
                                    .size(LabelSize::Small),
                            )
                            .child(
                                Label::new(state.explanation)
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            )
                    })),
            )
        })
        .into_any_element()
}

fn trace_arrow_label(kind: &str) -> &'static str {
    match kind {
        "move" | "partial_move" => "ownership moves",
        "copy" => "value copies",
        "clone" => "clone returns",
        "borrow_shared" => "shared reference",
        "borrow_mutable" | "borrow_activate" => "exclusive reference",
        "invalid_use" => "rejected use",
        "borrow_end" => "loan ends",
        "reinitialize" => "new value",
        "drop" => "drop",
        _ => "state",
    }
}

#[cfg(any())]
fn render_three_state_story(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
    verification: Option<&RepairVerification>,
    preview_repair_id: Option<&str>,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let Some(problem) = problem else {
        return empty_card("Select an issue to see its before, conflict, and repair states.");
    };
    let before = format!(
        "`{}` starts as an available binding. References point to its value; they do not become replacement owners.",
        problem.binding_name
    );
    let (_, _, rule) = problem_story(&problem.category, &problem.binding_name);
    let repair = preview_repair_id
        .and_then(|repair_id| model.repairs.iter().find(|repair| repair.id == repair_id));
    let (after_title, after_body, after_verified) = match verification.map(|state| &state.state) {
        Some(RepairVerificationState::Resolved { remaining_file_problems }) => (
            "Verified result",
            format!(
                "rustc confirmed this selected diagnostic is gone. {remaining_file_problems} other issue(s) may remain in this file."
            ),
            true,
        ),
        _ => match repair {
            Some(repair) => (
                "Hypothetical repair target",
                format!(
                    "If you apply “{}”, the intended topology changes as previewed below. This has not happened in the program yet.",
                    repair.title
                ),
                false,
            ),
            None => (
                "Repair target",
                "Choose a repair below to preview the intended ownership topology. Nothing is changed until you apply it, and success is only shown after rustc checks it."
                    .to_owned(),
                false,
            ),
        },
    };
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new("The state change in three pictures").size(LabelSize::Large))
        .child(render_state_card(
            "1 · Before",
            "Actual compiler/source fact",
            before,
            Color::Success,
            cx,
        ))
        .child(
            Label::new("↓ an operation changes who may access the value")
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .child(render_state_card(
            "2 · Conflict",
            "Actual rustc rejection",
            format!("{}\n\nRule being protected: {rule}", problem.message),
            Color::Error,
            cx,
        ))
        .child(
            Label::new("↓ select a design that matches your intent")
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .child(render_state_card(
            after_title,
            if after_verified {
                "Actual compiler-verified result"
            } else {
                "Preview, not current program state"
            },
            after_body,
            if after_verified {
                Color::Success
            } else {
                Color::Warning
            },
            cx,
        ))
        .into_any_element()
}

#[cfg(any())]
fn render_state_card(title: &str, badge: &str, body: String, color: Color, cx: &App) -> AnyElement {
    v_flex()
        .p_2()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(match color {
            Color::Error => cx.theme().status().error,
            Color::Success => cx.theme().status().success,
            _ => cx.theme().status().warning,
        })
        .child(
            Label::new(title.to_owned())
                .size(LabelSize::Small)
                .color(color),
        )
        .child(
            Label::new(badge.to_owned())
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .child(Label::new(body).size(LabelSize::Small))
        .into_any_element()
}

fn render_beginner_concept(
    problem: Option<&OwnershipProblem>,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let Some(problem) = problem else {
        return empty_card("A short core-rule explanation will appear for the selected issue.");
    };
    let concept_ids = learning_catalog::lesson_ids_for_problem(
        &problem.category,
        problem.diagnostic_code.as_deref(),
    );
    let Some(concept_id) = concept_ids.first() else {
        return empty_card(
            "The compiler facts above are available, but this issue has no bundled beginner explanation yet.",
        );
    };
    let Some(lesson) = learning_catalog::lesson(concept_id) else {
        return empty_card("The bundled beginner explanation could not be loaded.");
    };
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().status().info)
        .bg(cx.theme().status().info_background.opacity(0.08))
        .child(
            Label::new(format!("2 · Why Rust rejects this · {}", lesson.title))
                .size(LabelSize::Large),
        )
        .child(Label::new(lesson.one_line).size(LabelSize::Small))
        .child(Label::new(lesson.rule).size(LabelSize::Small))
        .child(
            Label::new(format!("Why: {}", lesson.why))
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .child(
            Label::new(format!("Picture: {}", lesson.memory_model))
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .child(
            Label::new(format!("Watch for: {}", lesson.misconception))
                .size(LabelSize::XSmall)
                .color(Color::Warning),
        )
        .into_any_element()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GuidedIssueFacts {
    target: String,
    headline: String,
    access_route: Option<String>,
    operation: Option<String>,
    required_access: Option<String>,
    state_summary: String,
}

fn guided_issue_facts(problem: &OwnershipProblem, model: &OwnershipModel) -> GuidedIssueFacts {
    let target = resolved_problem_target(problem, model).to_owned();
    let requirement = model.mutation_requirement.as_ref();
    let operation = selected_mutation_operation(model);
    let operation_name = requirement
        .map(|requirement| requirement.operation_name.clone())
        .or_else(|| operation.map(|operation| operation.name.clone()));
    let access_route = requirement.map(|requirement| requirement.access_source.clone());
    let required_access = requirement
        .map(|requirement| readable_access(&requirement.required_access))
        .or_else(|| operation.map(|operation| readable_access(&operation.required_access)))
        .map(str::to_owned);
    let headline = if problem.category == "immutable_mutation" {
        match (operation_name.as_deref(), access_route.as_deref()) {
            (Some(operation), Some(route)) => {
                format!("Cannot call {operation}() on `{target}` through `{route}`")
            }
            (Some(operation), None) => format!("Cannot call {operation}() on `{target}`"),
            _ => format!("`{target}` cannot be mutated through the current access path"),
        }
    } else {
        problem_story(&problem.category, &target).0
    };
    let state_summary = match problem.category.as_str() {
        "immutable_mutation" => format!(
            "`{target}` is alive and has not moved. The attempted write is blocked because no exclusive mutable access is available."
        ),
        "multiple_mutable_borrows"
        | "mutable_while_shared"
        | "assign_while_borrowed"
        | "use_while_mutably_borrowed"
        | "move_while_borrowed" => format!(
            "`{target}` is alive. A live borrow restricts the conflicting operation; borrowed does not mean dead."
        ),
        "use_after_move" | "partial_move" | "move_out_of_borrowed_content" => {
            let destination = model
                .value_trace
                .iter()
                .find_map(|step| step.to_label.as_deref())
                .map(|destination| format!(" The value now belongs to `{destination}`."))
                .unwrap_or_default();
            format!(
                "The old place `{target}` is unavailable after ownership transferred.{destination}"
            )
        }
        _ => format!(
            "Rust rejected this operation on `{target}` while preserving the value and access rules described below."
        ),
    };
    GuidedIssueFacts {
        target,
        headline,
        access_route,
        operation: operation_name,
        required_access,
        state_summary,
    }
}

fn render_visual_problem_header(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let Some(problem) = problem else {
        return empty_card(
            "Open a Rust file with a supported compiler diagnostic, then select the highlighted line.",
        );
    };
    let facts = guided_issue_facts(problem, model);
    let (_, what, _) = problem_story(&problem.category, &facts.target);
    let diagnostic = if problem.message.is_empty() {
        problem
            .diagnostic_code
            .clone()
            .unwrap_or_else(|| "compiler diagnostic".to_owned())
    } else {
        problem.message.clone()
    };
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().status().error)
        .bg(cx.theme().status().error_background.opacity(0.12))
        .child(
            h_flex()
                .gap_2()
                .justify_between()
                .child(
                    v_flex()
                        .gap_0p5()
                        .child(Label::new("1 · Problem").size(LabelSize::Large))
                        .child(Label::new(facts.headline.clone()).size(LabelSize::Small))
                        .child(
                            Label::new(format!(
                                "{} · {}",
                                problem.diagnostic_code.as_deref().unwrap_or("rustc"),
                                if problem.precision == "compiler_exact" {
                                    "compiler exact"
                                } else {
                                    "source estimate"
                                }
                            ))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                        ),
                )
                .child(
                    Button::new("visual-jump-to-error", "Show in code").on_click(cx.listener({
                        let range = problem.primary_range;
                        move |panel, _, _window, cx| panel.cue_range(range, cx)
                    })),
                ),
        )
        .child(Label::new(diagnostic).size(LabelSize::Small).color(Color::Error))
        .child(Label::new(what).size(LabelSize::Small))
        .child(
            h_flex()
                .gap_1()
                .flex_wrap()
                .child(Label::new(format!("Target · `{}`", facts.target)).size(LabelSize::XSmall))
                .when_some(facts.access_route, |this, route| {
                    this.child(Label::new(format!("Access route · `{route}`")).size(LabelSize::XSmall))
                })
                .when_some(facts.operation, |this, operation| {
                    this.child(Label::new(format!("Attempt · {operation}()")).size(LabelSize::XSmall))
                })
                .when_some(facts.required_access, |this, access| {
                    this.child(Label::new(format!("Needs · {access}")).size(LabelSize::XSmall))
                }),
        )
        .child(
            Label::new(facts.state_summary)
                .size(LabelSize::Small)
                .color(Color::Success),
        )
        .when(model.truncated, |this| {
            this.child(
                Label::new("The compiler model reached a display bound. Expand code context deliberately instead of rendering an unbounded graph.")
                    .size(LabelSize::XSmall)
                    .color(Color::Warning),
            )
        })
        .into_any_element()
}

fn render_visual_timeline(
    moments: &[VisualMoment],
    selected_step: usize,
    selected_moment: Option<VisualMoment>,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    if moments.is_empty() {
        return empty_card("Waiting for compiler and source facts for this diagnostic.");
    }
    v_flex()
        .p_3()
        .gap_3()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new("Ownership and control timeline").size(LabelSize::Large))
        .child(
            Label::new(
                "Choose a moment. The code highlight, memory nodes, and explanation move together.",
            )
            .size(LabelSize::XSmall)
            .color(Color::Muted),
        )
        .child(
            h_flex()
                .gap_1()
                .flex_wrap()
                .children(moments.iter().enumerate().map(|(index, moment)| {
                    let label = if index == selected_step {
                        format!("● {}", visual_phase_label(&moment.phase, index))
                    } else {
                        visual_phase_label(&moment.phase, index)
                    };
                    Button::new(SharedString::from(format!("visual-moment-{index}")), label)
                        .on_click(cx.listener(move |panel, _, _window, cx| {
                            panel.select_visual_step(index, cx);
                        }))
                })),
        )
        .when_some(selected_moment, |this, moment| {
            this.child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .rounded_md()
                    .border_1()
                    .border_color(visual_state_color(&moment.state, cx))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Label::new(visual_state_symbol(&moment.state))
                                    .size(LabelSize::Large)
                                    .color(visual_state_label_color(&moment.state)),
                            )
                            .child(Label::new(moment.title).size(LabelSize::Large)),
                    )
                    .child(Label::new(moment.explanation).size(LabelSize::Small))
                    .child(
                        Label::new(format!(
                            "Source: line {} · state: {}",
                            moment.range.start.line + 1,
                            moment.state.replace('_', " ")
                        ))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                    ),
            )
        })
        .into_any_element()
}

fn visual_phase_label(phase: &str, index: usize) -> String {
    let label = match phase {
        "borrow_created" | "contract" => "Before",
        "operation_rejected" | "invalid_use" => "Conflict",
        "borrow_ended" | "repair" => "After / repair",
        "move" => "Move",
        "partial_move" => "Partial move",
        "borrow_shared" => "Shared borrow",
        "borrow_mutable" | "borrow_activate" => "Mutable borrow",
        "borrow_end" => "Borrow ends",
        "reinitialize" => "Reinitialize",
        "drop" => "Drop",
        _ => "Step",
    };
    format!("{} · {label}", index + 1)
}

fn visual_state_symbol(state: &str) -> &'static str {
    if state.contains("reject") || state.contains("invalid") {
        "!"
    } else if state.contains("drop") {
        "×"
    } else if state.contains("move") {
        "○"
    } else if state.contains("borrow") {
        "◇"
    } else if state.contains("available") {
        "●"
    } else {
        "◆"
    }
}

fn visual_state_label_color(state: &str) -> Color {
    if state.contains("reject") || state.contains("invalid") {
        Color::Error
    } else if state.contains("borrow") {
        Color::Info
    } else if state.contains("available") {
        Color::Success
    } else if state.contains("move") || state.contains("drop") {
        Color::Warning
    } else {
        Color::Muted
    }
}

fn visual_state_color(state: &str, cx: &App) -> gpui::Hsla {
    if state.contains("reject") || state.contains("invalid") {
        cx.theme().status().error
    } else if state.contains("borrow") {
        cx.theme().status().info
    } else if state.contains("available") {
        cx.theme().status().success
    } else {
        cx.theme().status().warning
    }
}

fn render_visual_memory_map(
    model: &OwnershipModel,
    selected_step: usize,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    if let Some(graph) = &model.conflict_graph {
        let snapshot = graph
            .snapshots
            .get(selected_step)
            .or_else(|| graph.snapshots.last());
        return v_flex()
            .p_3()
            .gap_3()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .child(Label::new("Pointers and ownership right now").size(LabelSize::Large))
            .child(
                Label::new("Actual compiler/source model · a borrowed value is still alive; only access is restricted")
                    .size(LabelSize::XSmall)
                    .color(Color::Success),
            )
            .child(
                Label::new(graph.summary.clone())
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(
                v_flex()
                    .gap_2()
                    .children(graph.nodes.clone().into_iter().map(|node| {
                        let node_state = snapshot.and_then(|snapshot| {
                            snapshot.states.iter().find(|state| state.node_id == node.id)
                        });
                        render_visual_memory_node(node, node_state, cx)
                    })),
            )
            .child(Label::new("Relationships").size(LabelSize::Small))
            .children(graph.edges.clone().into_iter().filter_map(|edge| {
                let from = graph.nodes.iter().find(|node| node.id == edge.from)?;
                let to = graph.nodes.iter().find(|node| node.id == edge.to)?;
                Some(
                    h_flex()
                        .p_2()
                        .gap_1()
                        .rounded_md()
                        .bg(cx.theme().status().info_background.opacity(0.12))
                        .child(Label::new(format!("{}  ──{}──►  {}", from.label, edge.label, to.label)).size(LabelSize::Small))
                        .child(provenance_badge(&edge.provenance)),
                )
            }))
            .children(model.bindings.iter().take(2).map(|binding| {
                render_smart_pointer_shape(&binding.name, &binding.type_name, cx)
            }))
            .child(
                Label::new("A reference and the value it points to are separate nodes. Borrowed values remain alive; the map marks only the access that is restricted.")
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .into_any_element();
    }

    if model.bindings.is_empty()
        && let (Some(requirement), Some(operation)) = (
            model.mutation_requirement.as_ref(),
            selected_mutation_operation(model),
        )
    {
        let receiver_type = operation
            .receiver_type
            .as_deref()
            .unwrap_or("resolved receiver type");
        return v_flex()
            .p_3()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .child(Label::new("Pointers and storage").size(LabelSize::Large))
            .child(
                Label::new("Structured source and signature facts · no runtime addresses or counts are assumed")
                    .size(LabelSize::XSmall)
                    .color(Color::Success),
            )
            .child(
                Label::new(format!(
                    "[ shared reference `{}` ]  ──►  [ live owner ]  ──►  [ field `{}`: {receiver_type} ]",
                    requirement.access_source, requirement.target_place
                ))
                .size(LabelSize::Small)
                .buffer_font(cx),
            )
            .child(render_smart_pointer_shape(
                &requirement.target_place,
                receiver_type,
                cx,
            ))
            .child(
                Label::new(format!(
                    "`{}` requires {}, so the field and its storage remain alive while this write is rejected.",
                    operation.name,
                    readable_access(&requirement.required_access)
                ))
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .into_any_element();
    }

    if model.bindings.is_empty() {
        return empty_card(
            "No compiler memory-layout facts are attached to this diagnostic. The debugger will not invent stack or heap nodes.",
        );
    }
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new("Pointers and ownership right now").size(LabelSize::Large))
        .child(
            Label::new("Actual compiler representation facts · counts are symbolic, not sampled runtime values")
                .size(LabelSize::XSmall)
                .color(Color::Success),
        )
        .children(model.bindings.clone().into_iter().map(|binding| {
            v_flex()
                .p_2()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    Button::new(
                        SharedString::from(format!("visual-binding-{}", binding.id)),
                        format!("▣ stack binding `{}`: {}", binding.name, binding.type_name),
                    )
                    .on_click(cx.listener({
                        let range = binding.range;
                        move |panel, _, _window, cx| panel.cue_range(range, cx)
                    })),
                )
                .child(render_smart_pointer_shape(&binding.name, &binding.type_name, cx))
                .children(binding.memory_layers.into_iter().map(|layer| {
                    Label::new(format!(
                        "{} ─► {} · {}",
                        memory_symbol(&layer.storage),
                        layer.label,
                        beginner_memory_explanation(&layer.kind)
                    ))
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                }))
        }))
        .into_any_element()
}

fn render_smart_pointer_shape(name: &str, type_name: &str, cx: &App) -> AnyElement {
    let nodes = smart_pointer_nodes(name, type_name);

    v_flex()
        .p_2()
        .gap_1()
        .rounded_md()
        .bg(cx.theme().status().info_background.opacity(0.08))
        .children(nodes.into_iter().enumerate().flat_map(|(index, node)| {
            let arrow = (index > 0).then(|| {
                Label::new("             ↓ points to / contains")
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .into_any_element()
            });
            arrow.into_iter().chain(std::iter::once(
                Label::new(format!("[ {node} ]"))
                    .size(LabelSize::XSmall)
                    .buffer_font(cx)
                    .into_any_element(),
            ))
        }))
        .into_any_element()
}

fn smart_pointer_nodes(name: &str, type_name: &str) -> Vec<String> {
    if type_name.contains("Rc<RefCell<") {
        vec![
            format!("stack · Rc handle `{name}`"),
            "shared heap allocation · strong count = symbolic N".to_owned(),
            "RefCell · runtime borrow flag".to_owned(),
            "inner value T".to_owned(),
        ]
    } else if type_name.contains("Arc<Mutex<") {
        vec![
            format!("stack/thread · Arc handle `{name}`"),
            "shared heap allocation · atomic strong count = symbolic N".to_owned(),
            "Mutex · one lock holder at a time".to_owned(),
            "inner value T".to_owned(),
        ]
    } else if type_name.contains("Arc<RwLock<") {
        vec![
            format!("stack/thread · Arc handle `{name}`"),
            "shared heap allocation · atomic strong count = symbolic N".to_owned(),
            "RwLock · many readers or one writer".to_owned(),
            "inner value T".to_owned(),
        ]
    } else if type_name.contains("Rc<") {
        vec![
            format!("stack · Rc handle `{name}`"),
            "shared heap allocation · strong count = symbolic N".to_owned(),
            "inner value T · Rc move: N unchanged; Rc::clone: N + 1".to_owned(),
        ]
    } else if type_name.contains("Arc<") {
        vec![
            format!("stack/thread · Arc handle `{name}`"),
            "shared heap allocation · atomic strong count = symbolic N".to_owned(),
            "inner value T · Arc move: N unchanged; Arc::clone: N + 1".to_owned(),
        ]
    } else if type_name.contains("RefCell<") {
        vec![
            format!("stack/owner · RefCell `{name}`"),
            "runtime borrow flag · 0, readers, or one writer".to_owned(),
            "inner value T · conflicting borrow can panic".to_owned(),
        ]
    } else if type_name.starts_with("&mut ") {
        vec![
            format!("reference `{name}` · non-owning pointer"),
            "exclusive access for this loan".to_owned(),
            "borrowed value remains alive at its owner".to_owned(),
        ]
    } else if type_name.starts_with('&') {
        vec![
            format!("reference `{name}` · non-owning pointer"),
            "shared read access for this loan".to_owned(),
            "borrowed value remains alive at its owner".to_owned(),
        ]
    } else if type_name.contains("Box<") {
        vec![
            format!("stack · unique Box handle `{name}`"),
            "heap · one owned allocation".to_owned(),
            "inner value T · moving Box transfers the handle, not the allocation".to_owned(),
        ]
    } else if type_name.contains("Vec<") || type_name == "String" {
        vec![
            format!("stack · `{name}` handle (pointer, length, capacity)"),
            "heap · element buffer".to_owned(),
            "moving the handle leaves the buffer in place".to_owned(),
        ]
    } else {
        vec![
            format!("binding `{name}` · {type_name}"),
            "value representation tracked by rustc".to_owned(),
        ]
    }
}

fn render_visual_memory_node(
    node: rust_analyzer_ext::OwnershipConflictNode,
    state: Option<&rust_analyzer_ext::OwnershipConflictNodeState>,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let state_text = state.map_or("alive", |state| state.state.as_str());
    let label = format!(
        "{} {} · {}",
        visual_state_symbol(state_text),
        node.label,
        node.type_name.as_deref().unwrap_or("type not available")
    );
    v_flex()
        .p_2()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(visual_state_color(state_text, cx))
        .child(if let Some(range) = node.range {
            Button::new(
                SharedString::from(format!("memory-node-{}", node.id)),
                label,
            )
            .on_click(cx.listener(move |panel, _, _window, cx| panel.cue_range(range, cx)))
            .into_any_element()
        } else {
            Label::new(label).size(LabelSize::Small).into_any_element()
        })
        .child(
            Label::new(format!("{} · {state_text}", node.memory))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .when_some(state, |this, state| {
            this.child(
                Label::new(state.explanation.clone())
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
        })
        .into_any_element()
}

fn render_codebase_context(
    model: &OwnershipModel,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let Some(context) = &model.source_context else {
        return empty_card(
            "Codebase context will appear after rust-analyzer resolves the selected source location.",
        );
    };
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new("Where this sits in the codebase").size(LabelSize::Large))
        .child(
            h_flex()
                .gap_1()
                .flex_wrap()
                .children(context.breadcrumbs.clone().into_iter().enumerate().map(
                    |(index, item)| {
                        let text = if index == 0 {
                            format!("{}: {}", item.kind, item.label)
                        } else {
                            format!("→ {}: {}", item.kind, item.label)
                        };
                        if let Some(range) = item.range {
                            Button::new(
                                SharedString::from(format!("context-item-{index}")),
                                text,
                            )
                            .on_click(cx.listener(move |panel, _, _window, cx| {
                                panel.cue_range(range, cx);
                            }))
                            .into_any_element()
                        } else {
                            Label::new(text).size(LabelSize::Small).into_any_element()
                        }
                    },
                )),
        )
        .when(!context.call_paths.is_empty(), |this| {
            this.child(Label::new("Resolved workspace call paths").size(LabelSize::Small))
                .children(context.call_paths.clone().into_iter().map(|path| {
                    Label::new(format!("• {}", path.join(" → ")))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted)
                }))
        })
        .when(!context.related_types.is_empty(), |this| {
            this.child(Label::new(format!(
                "Types involved: {}",
                context.related_types.join(" · ")
            )).size(LabelSize::XSmall).color(Color::Muted))
        })
        .when(context.truncated, |this| {
            this.child(
                Label::new("More relationships exist. The display is intentionally bounded to keep navigation responsive.")
                    .size(LabelSize::XSmall)
                    .color(Color::Warning),
            )
        })
        .child(provenance_badge(&context.provenance))
        .into_any_element()
}

#[cfg(any())]
fn render_visual_operation_summary(
    model: &OwnershipModel,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    if model.operations.is_empty() {
        return empty_card(
            "No resolved method or function contract is directly involved in this diagnostic.",
        );
    }
    let hidden = model.operations.len().saturating_sub(3);
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new("What the involved operations require").size(LabelSize::Large))
        .children(model.operations.iter().take(3).cloned().map(|operation| {
            let range = operation.range;
            let facts = if operation.effect_facts.is_empty() {
                operation
                    .effects
                    .iter()
                    .take(2)
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                operation
                    .effect_facts
                    .iter()
                    .take(2)
                    .map(|effect| effect.summary.clone())
                    .collect::<Vec<_>>()
            };
            v_flex()
                .p_2()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    h_flex()
                        .gap_2()
                        .justify_between()
                        .child(Label::new(format!("{}()", operation.name)).size(LabelSize::Small))
                        .child(
                            Button::new(
                                SharedString::from(format!("visual-operation-{}", operation.id)),
                                format!("line {}", range.start.line + 1),
                            )
                            .on_click(cx.listener(
                                move |panel, _, _window, cx| {
                                    panel.cue_range(range, cx);
                                },
                            )),
                        ),
                )
                .child(
                    Label::new(operation.signature)
                        .size(LabelSize::XSmall)
                        .buffer_font(cx),
                )
                .child(
                    Label::new(format!(
                        "Needs {}. {}",
                        readable_access(&operation.required_access),
                        operation.why_required
                    ))
                    .size(LabelSize::XSmall),
                )
                .children(facts.into_iter().map(|fact| {
                    Label::new(format!("• {fact}"))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted)
                }))
                .child(provenance_badge(&operation.provenance))
        }))
        .when(hidden > 0, |this| {
            this.child(
                Label::new(format!(
                    "+{hidden} more resolved operations are available under Advanced compiler evidence."
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            )
        })
        .into_any_element()
}

#[cfg(any())]
fn render_repair_idea_cards(
    problem: Option<&OwnershipProblem>,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let Some(problem) = problem else {
        return empty_card(
            "Select a compiler diagnostic to compare intent-level repair strategies.",
        );
    };
    let ideas = learning_catalog::repair_ideas(&problem.category);
    if ideas.is_empty() {
        return empty_card(
            "No prewritten intent patterns match this diagnostic family. Standard compiler actions remain available from the editor.",
        );
    }
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new("Intent-level repair choices").size(LabelSize::Large))
        .child(
            Label::new("These are prewritten design choices, not automatic edits. Compiler-validated source diffs are shown separately below.")
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .children(ideas.iter().enumerate().map(|(index, idea)| {
            v_flex()
                .p_2()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(Label::new(format!("{} · {}", index + 1, idea.title)).size(LabelSize::Small))
                .child(Label::new(idea.intent).size(LabelSize::XSmall))
                .child(
                    Label::new(format!("Tradeoff: {}", idea.tradeoff))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
        }))
        .into_any_element()
}

#[cfg(any())]
fn render_concept_map(
    problem: Option<&OwnershipProblem>,
    selected_concept_id: Option<&str>,
    checkpoint_choice: Option<usize>,
    progress: &LearningProgress,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let current = selected_concept_id.or_else(|| {
        problem.and_then(|problem| {
            learning_catalog::lesson_ids_for_problem(
                &problem.category,
                problem.diagnostic_code.as_deref(),
            )
            .first()
            .copied()
        })
    });
    v_flex()
        .gap_3()
        .child(
            v_flex()
                .p_3()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(Label::new("My Rust concept map").size(LabelSize::Large))
                .child(
                    Label::new("Progress is stored only in Zed's local workspace database. A concept advances after a compiler-verified fix and an optional checkpoint.")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
        )
        .child(
            v_flex()
                .p_3()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .children(learning_catalog::all_lessons().iter().map(|lesson| {
                    let concept_progress = progress.concepts.get(lesson.id).cloned().unwrap_or_default();
                    let selected = current == Some(lesson.id);
                    h_flex()
                        .gap_2()
                        .justify_between()
                        .child(
                            Button::new(
                                SharedString::from(format!("concept-map-{}", lesson.id)),
                                if selected {
                                    format!("● {}", lesson.title)
                                } else {
                                    lesson.title.to_owned()
                                },
                            )
                            .on_click(cx.listener({
                                let concept_id = lesson.id.to_owned();
                                move |panel, _, _window, cx| {
                                    panel.select_concept(concept_id.clone(), cx);
                                }
                            })),
                        )
                        .child(
                            Label::new(format!(
                                "{}  {}",
                                mastery_meter(&concept_progress),
                                concept_progress.level()
                            ))
                            .size(LabelSize::XSmall)
                            .color(mastery_color(&concept_progress)),
                        )
                })),
        )
        .when_some(current, |this, concept_id| {
            this.child(render_concept_lesson(
                concept_id,
                checkpoint_choice,
                progress,
                false,
                cx,
            ))
        })
        .into_any_element()
}

#[cfg(any())]
fn mastery_meter(progress: &ConceptProgress) -> &'static str {
    if progress.checkpoint_passed && progress.verified_fixes > 0 {
        "█████"
    } else if progress.verified_fixes > 0 {
        "████░"
    } else if progress.encounters > 1 {
        "███░░"
    } else if progress.encounters == 1 {
        "█░░░░"
    } else {
        "░░░░░"
    }
}

#[cfg(any())]
fn mastery_color(progress: &ConceptProgress) -> Color {
    if progress.checkpoint_passed && progress.verified_fixes > 0 {
        Color::Success
    } else if progress.encounters > 0 {
        Color::Info
    } else {
        Color::Muted
    }
}

#[cfg(any())]
fn render_concept_lesson(
    concept_id: &str,
    checkpoint_choice: Option<usize>,
    progress: &LearningProgress,
    compact: bool,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let Some(lesson) = learning_catalog::lesson(concept_id) else {
        return empty_card("This concept is not present in the bundled explanation catalog.");
    };
    let concept_progress = progress
        .concepts
        .get(concept_id)
        .cloned()
        .unwrap_or_default();
    let correct = checkpoint_choice.is_some_and(|choice| choice == lesson.correct_choice);
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().status().info)
        .bg(cx.theme().status().info_background.opacity(0.1))
        .child(
            h_flex()
                .gap_2()
                .justify_between()
                .child(Label::new(format!("Core concept · {}", lesson.title)).size(LabelSize::Large))
                .child(
                    Label::new(format!(
                        "{} {}",
                        mastery_meter(&concept_progress),
                        concept_progress.level()
                    ))
                    .size(LabelSize::XSmall)
                    .color(mastery_color(&concept_progress)),
                ),
        )
        .child(Label::new(lesson.one_line).size(LabelSize::Small))
        .child(Label::new(format!("Rule: {}", lesson.rule)).size(LabelSize::Small))
        .child(
            Label::new(format!("Memory model: {}", lesson.memory_model))
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .when(!compact, |this| {
            this.child(Label::new(format!("Why Rust cares: {}", lesson.why)).size(LabelSize::Small))
                .child(
                    Label::new(format!("Common misconception: {}", lesson.misconception))
                        .size(LabelSize::Small)
                        .color(Color::Warning),
                )
        })
        .child(Label::new(format!("Checkpoint: {}", lesson.checkpoint)).size(LabelSize::Small))
        .child(
            h_flex()
                .gap_1()
                .flex_wrap()
                .children(lesson.choices.iter().enumerate().map(|(index, choice)| {
                    let label = if checkpoint_choice == Some(index) {
                        format!("● {choice}")
                    } else {
                        (*choice).to_owned()
                    };
                    Button::new(
                        SharedString::from(format!("checkpoint-{concept_id}-{index}")),
                        label,
                    )
                    .on_click(cx.listener({
                        let concept_id = concept_id.to_owned();
                        move |panel, _, _window, cx| {
                            panel.answer_checkpoint(&concept_id, index, cx);
                        }
                    }))
                })),
        )
        .when(checkpoint_choice.is_some(), |this| {
            this.child(
                Label::new(if correct {
                    "Correct. The concept map records this checkpoint locally."
                } else {
                    "Not quite. Re-read the rule and memory model, then try again."
                })
                .size(LabelSize::XSmall)
                .color(if correct { Color::Success } else { Color::Warning }),
            )
        })
        .when(!lesson.related.is_empty(), |this| {
            this.child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .child(Label::new("Related:").size(LabelSize::XSmall).color(Color::Muted))
                    .children(lesson.related.iter().filter_map(|related_id| {
                        let related = learning_catalog::lesson(related_id)?;
                        Some(
                            Button::new(
                                SharedString::from(format!("related-concept-{}", related.id)),
                                related.title,
                            )
                            .on_click(cx.listener({
                                let concept_id = related.id.to_owned();
                                move |panel, _, _window, cx| {
                                    panel.select_concept(concept_id.clone(), cx);
                                }
                            })),
                        )
                    })),
            )
        })
        .child(
            Label::new("Explanation source: bundled, versioned Rust learning catalog. Concrete states and source locations above come from rustc/rust-analyzer.")
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .into_any_element()
}

fn render_learning_section(
    section: LearningSection,
    collapsed: bool,
    render_content: impl FnOnce(&mut Context<RustWorkbenchPanel>) -> AnyElement,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let content = (!collapsed).then(|| render_content(cx));
    v_flex()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(
            h_flex()
                .p_3()
                .gap_2()
                .justify_between()
                .child(
                    v_flex()
                        .gap_0p5()
                        .child(Label::new(section.title()).size(LabelSize::Large))
                        .child(
                            Label::new(section.subtitle())
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        ),
                )
                .child(
                    Button::new(
                        SharedString::from(format!("toggle-learning-section-{section:?}")),
                        if collapsed { "Expand" } else { "Collapse" },
                    )
                    .on_click(cx.listener(move |panel, _, _window, cx| {
                        panel.toggle_learning_section(section, cx);
                    })),
                ),
        )
        .when_some(content, |this, content| {
            this.child(
                v_flex()
                    .p_3()
                    .border_t_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(content),
            )
        })
        .into_any_element()
}

fn render_operation_insights(
    model: &OwnershipModel,
    expanded_operations: &BTreeSet<String>,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    if model.operations.is_empty() {
        return empty_card(
            "No resolved function call was attached to this selected compiler flow. The ownership explanation above still comes from rustc.",
        );
    }
    v_flex()
        .gap_2()
        .child(Label::new("Operations involved in this error").size(LabelSize::Large))
        .child(
            Label::new("These cards come from resolved Rust signatures. Workspace-local bodies are followed through a bounded call graph; opaque library bodies are described from their public contract, documentation, and a conservative standard-library catalog.")
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .children(model.operations.clone().into_iter().map(|operation| {
            let range = operation.range;
            let operation_id = operation.id.clone();
            let expanded = expanded_operations.contains(&operation.id);
            let alternative_count = operation.alternatives.len();
            let visible_alternatives = if expanded { alternative_count } else { 3 };
            let effect_rows = if operation.effect_facts.is_empty() {
                operation
                    .effects
                    .iter()
                    .map(|effect| ("behavior".to_owned(), effect.clone(), operation.provenance.clone()))
                    .collect::<Vec<_>>()
            } else {
                operation
                    .effect_facts
                    .iter()
                    .map(|effect| {
                        (
                            effect.kind.clone(),
                            effect.summary.clone(),
                            effect.certainty.clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            v_flex()
                .p_3()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    h_flex()
                        .gap_2()
                        .justify_between()
                        .child(Label::new(format!("{}()", operation.name)).size(LabelSize::Large))
                        .child(
                            Button::new(
                                SharedString::from(format!("show-operation-{}", operation.id)),
                                format!("line {}", operation.range.start.line + 1),
                            )
                            .on_click(cx.listener(move |panel, _, _window, cx| {
                                panel.cue_range(range, cx);
                            })),
                        ),
                )
                .child(
                    Label::new(operation.signature)
                        .size(LabelSize::Small)
                        .buffer_font(cx),
                )
                .child(
                    Label::new(format!(
                        "Requires: {}  ·  Receiver: {}",
                        readable_access(&operation.required_access),
                        operation.receiver_type.as_deref().unwrap_or("free function")
                    ))
                    .size(LabelSize::Small),
                )
                .child(
                    Label::new(operation.why_required)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .child(
                    Label::new(format!("At this call site: {}", operation.available_access))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
                .when_some(operation.documentation, |this, documentation| {
                    this.child(
                        Label::new(format!("API intent: {documentation}"))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                })
                .child(Label::new("Observable effects").size(LabelSize::Small))
                .children(effect_rows.into_iter().map(|(kind, summary, certainty)| {
                    v_flex()
                        .p_2()
                        .gap_0p5()
                        .rounded_md()
                        .bg(cx.theme().status().info_background.opacity(0.08))
                        .child(Label::new(format!("{} · {summary}", kind.replace('_', " "))).size(LabelSize::XSmall))
                        .child(Label::new(format!("Evidence: {certainty}")).size(LabelSize::XSmall).color(Color::Muted))
                }))
                .when(operation.call_chain.len() > 1, |this| {
                    this.child(
                        Label::new(format!(
                            "Workspace call path: {}",
                            operation.call_chain.join(" → ")
                        ))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                    )
                })
                .when(operation.truncated, |this| {
                    this.child(
                        Label::new("The body walk reached its safety bound; additional effects are intentionally reported as unknown.")
                            .size(LabelSize::XSmall)
                            .color(Color::Warning),
                    )
                })
                .when(alternative_count > 0, |this| {
                    this.child(Label::new("Related operations (not automatic fixes)").size(LabelSize::Small))
                        .children(
                            operation
                                .alternatives
                                .into_iter()
                                .take(visible_alternatives)
                                .map(|alternative| {
                                    v_flex()
                                        .p_2()
                                        .gap_1()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(cx.theme().colors().border_variant)
                                        .child(
                                            Label::new(format!(
                                                "{} · {} · {}",
                                                alternative.signature,
                                                readable_access(&alternative.access),
                                                alternative.behavior
                                            ))
                                            .size(LabelSize::XSmall),
                                        )
                                        .child(
                                            Label::new(alternative.difference)
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted),
                                        )
                                }),
                        )
                        .when(alternative_count > 3, |this| {
                            this.child(
                                Button::new(
                                    SharedString::from(format!("expand-operation-{operation_id}")),
                                    if expanded {
                                        "Show fewer related operations"
                                    } else {
                                        "Show all known related operations"
                                    },
                                )
                                .on_click(cx.listener(move |panel, _, _window, cx| {
                                    panel.toggle_operation_details(operation_id.clone(), cx);
                                })),
                            )
                        })
                })
                .child(provenance_badge(&operation.provenance))
        }))
        .into_any_element()
}

fn readable_access(access: &str) -> &'static str {
    match access {
        "shared_borrow" => "shared borrow (&self / &T)",
        "mutable_borrow" => "exclusive mutable borrow (&mut self / &mut T)",
        "move" => "ownership (self / T)",
        _ => "the declared parameter contracts",
    }
}

fn readable_available_access(access: &str) -> &'static str {
    match access {
        "shared_borrow" => "shared access",
        "immutable_binding" => "an immutable binding",
        "shared_owner" => "shared-owner access",
        "mutable_receiver_with_blocked_path" => "a mutable receiver with a blocked inner path",
        "owned_receiver_with_blocked_path" => "an owned receiver with a blocked inner path",
        _ => "insufficient mutable access",
    }
}

fn display_profile_label(profile: RustOwnershipDisplayProfile) -> &'static str {
    match profile {
        RustOwnershipDisplayProfile::Focus => "Focus",
        RustOwnershipDisplayProfile::Learn => "Learn",
        RustOwnershipDisplayProfile::Full => "Full",
        RustOwnershipDisplayProfile::Custom => "Custom",
    }
}

fn selected_button_label(selected: bool, label: &'static str) -> String {
    if selected {
        format!("● {label}")
    } else {
        label.to_owned()
    }
}

fn render_display_controls(
    preferences: &RustOwnershipDisplayPreferences,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let profile = preferences.profile;
    let inline_mode = preferences.inline_diagnostics;
    let scope = preferences.scope;
    let filters = [
        ("types", "Types", preferences.show_type_hints),
        ("parameters", "Parameters", preferences.show_parameter_hints),
        ("other", "Other", preferences.show_other_hints),
        ("adjustments", "Adjustments", preferences.show_adjustments),
        ("lifetimes", "Lifetimes", preferences.show_lifetimes),
        ("moves", "Moves", preferences.show_moves),
        ("borrows", "Borrows", preferences.show_borrows),
        (
            "invalid_uses",
            "Invalid uses",
            preferences.show_invalid_uses,
        ),
        ("last_uses", "Last uses", preferences.show_last_uses),
        ("borrow_ends", "Borrow ends", preferences.show_borrow_ends),
        (
            "reinitializations",
            "Reinitializations",
            preferences.show_reinitializations,
        ),
        ("drops", "Drops", preferences.show_drops),
        (
            "ownership_colors",
            "Ownership colors",
            preferences.show_ownership_coloring,
        ),
    ];
    v_flex()
        .px_3()
        .py_2()
        .gap_2()
        .border_b_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new("Choose how much Rust explains in the editor").size(LabelSize::Small))
        .child(
            Label::new("Focus is quiet; Learn follows the selected ownership story; Full shows every available compiler and analyzer cue.")
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .child(
            h_flex().gap_1().flex_wrap().children([
                RustOwnershipDisplayProfile::Focus,
                RustOwnershipDisplayProfile::Learn,
                RustOwnershipDisplayProfile::Full,
            ].into_iter().map(|candidate| {
                Button::new(
                    SharedString::from(format!("display-profile-{candidate:?}")),
                    selected_button_label(candidate == profile, display_profile_label(candidate)),
                )
                .on_click(cx.listener(move |panel, _, _window, cx| {
                    panel.set_display_profile(candidate, cx);
                }))
            })),
        )
        .child(Label::new("Inline compiler message").size(LabelSize::XSmall).color(Color::Muted))
        .child(
            h_flex().gap_1().flex_wrap().children([
                (RustInlineDiagnosticMode::Off, "Off"),
                (RustInlineDiagnosticMode::Selected, "Selected issue"),
                (RustInlineDiagnosticMode::All, "All"),
            ].into_iter().map(|(candidate, label)| {
                Button::new(
                    SharedString::from(format!("inline-mode-{candidate:?}")),
                    selected_button_label(candidate == inline_mode, label),
                )
                .on_click(cx.listener(move |panel, _, _window, cx| {
                    panel.display_preferences.inline_diagnostics = candidate;
                    panel.display_preferences.profile = RustOwnershipDisplayProfile::Custom;
                    panel.display_preferences_changed(cx);
                }))
            })),
        )
        .child(Label::new("Ownership scope").size(LabelSize::XSmall).color(Color::Muted))
        .child(
            h_flex().gap_1().flex_wrap().children([
                (RustOwnershipHintScope::SelectedBinding, "Selected binding"),
                (RustOwnershipHintScope::CurrentFunction, "Current function"),
                (RustOwnershipHintScope::File, "File"),
            ].into_iter().map(|(candidate, label)| {
                Button::new(
                    SharedString::from(format!("ownership-scope-{candidate:?}")),
                    selected_button_label(candidate == scope, label),
                )
                .on_click(cx.listener(move |panel, _, _window, cx| {
                    panel.display_preferences.scope = candidate;
                    panel.display_preferences.profile = RustOwnershipDisplayProfile::Custom;
                    panel.display_preferences_changed(cx);
                }))
            })),
        )
        .child(Label::new("Hint categories").size(LabelSize::XSmall).color(Color::Muted))
        .child(h_flex().gap_1().flex_wrap().children(filters.into_iter().map(
            |(filter, label, enabled)| {
                Button::new(
                    SharedString::from(format!("ownership-filter-{filter}")),
                    selected_button_label(enabled, label),
                )
                .on_click(cx.listener(move |panel, _, _window, cx| {
                    panel.toggle_display_filter(filter, cx);
                }))
            },
        )))
        .into_any_element()
}

#[cfg(any())]
fn render_coach_problem(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
    show_why_rust_cares: bool,
    show_core_concept: bool,
    show_runtime_cost: bool,
    show_full_conflict_graph: bool,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let Some(problem) = problem else {
        if model.events.is_empty() {
            return empty_card(
                "No ownership problem is selected yet. Save the file and wait for Cargo check, or place the cursor on a value to explore valid ownership flow.",
            );
        }
        return v_flex()
            .p_3()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().status().success)
            .child(Label::new("This selected value is accepted by Rust").size(LabelSize::Large))
            .child(
                Label::new("Use the Flow step to see moves, borrows, last uses, and drops that the compiler recorded.")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .into_any_element();
    };
    let (fallback_title, fallback_what_happened, why_rejected) =
        problem_story(&problem.category, &problem.binding_name);
    let (title, what_happened) = model
        .conflict_graph
        .as_ref()
        .map_or((fallback_title, fallback_what_happened), |graph| {
            (graph.title.clone(), graph.summary.clone())
        });
    let lesson = core_concept_lesson(&problem.category, &problem.binding_name);
    let binding_range = problem.binding_range;
    let primary_range = problem.primary_range;
    v_flex()
        .p_3()
        .gap_3()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().status().error)
        .child(Label::new(title).size(LabelSize::Large))
        .child(provenance_badge(&problem.precision))
        .when_some(model.conflict_graph.as_ref(), |this, graph| {
            this.child(render_conflict_graph(graph, show_full_conflict_graph, cx))
        })
        .child(
            v_flex()
                .p_2()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(Label::new("The rule").size(LabelSize::Small))
                .child(
                    Label::new(lesson.rule.clone())
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
        )
        .child(
            v_flex()
                .p_2()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(Label::new("Before → operation → after").size(LabelSize::Small))
                .child(
                    Label::new(format!(
                        "{}  →  {}  →  {}",
                        lesson.before, lesson.operation, lesson.after
                    ))
                    .size(LabelSize::Small)
                    .buffer_font(cx),
                )
                .child(
                    Label::new(lesson.usable.clone())
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
        )
        .child(
            v_flex()
                .gap_1()
                .child(Label::new("What happened").size(LabelSize::Small))
                .child(
                    Label::new(what_happened)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
        )
        .child(
            v_flex()
                .gap_1()
                .child(Label::new("Why Rust stopped here").size(LabelSize::Small))
                .child(
                    Label::new(why_rejected)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("show-owner-declaration", "Show where it was created").on_click(
                        cx.listener(move |panel, _, _window, cx| {
                            panel.cue_range(binding_range, cx);
                        }),
                    ),
                )
                .child(
                    Button::new("show-rejected-use", "Show rejected use").on_click(cx.listener(
                        move |panel, _, _window, cx| {
                            panel.cue_range(primary_range, cx);
                        },
                    )),
                ),
        )
        .child(
            h_flex()
                .gap_1()
                .flex_wrap()
                .child(
                    Button::new(
                        "why-rust-cares",
                        if show_why_rust_cares {
                            "Hide C risk"
                        } else {
                            "Why Rust cares"
                        },
                    )
                    .on_click(cx.listener(|panel, _, _window, cx| {
                        panel.show_why_rust_cares = !panel.show_why_rust_cares;
                        cx.notify();
                    })),
                )
                .child(
                    Button::new(
                        "core-concept",
                        if show_core_concept {
                            "Hide concept"
                        } else {
                            "Core concept"
                        },
                    )
                    .on_click(cx.listener(|panel, _, _window, cx| {
                        panel.show_core_concept = !panel.show_core_concept;
                        cx.notify();
                    })),
                )
                .child(
                    Button::new(
                        "runtime-cost",
                        if show_runtime_cost {
                            "Hide runtime cost"
                        } else {
                            "Memory/runtime cost"
                        },
                    )
                    .on_click(cx.listener(|panel, _, _window, cx| {
                        panel.show_runtime_cost = !panel.show_runtime_cost;
                        cx.notify();
                    })),
                ),
        )
        .when(show_why_rust_cares, |this| {
            this.child(explanation_card(
                "Why Rust cares (C comparison)",
                lesson.c_risk.clone(),
                cx,
            ))
        })
        .when(show_core_concept, |this| {
            this.child(
                v_flex()
                    .p_2()
                    .gap_1()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(Label::new(lesson.concept_title.clone()).size(LabelSize::Small))
                    .child(
                        Label::new(lesson.concept.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(lesson.example.clone())
                            .size(LabelSize::XSmall)
                            .buffer_font(cx),
                    )
                    .child(
                        Label::new(format!("Common misconception: {}", lesson.misconception))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
        })
        .when(show_runtime_cost, |this| {
            this.child(explanation_card(
                "Memory and runtime cost",
                lesson.runtime.clone(),
                cx,
            ))
        })
        .when_some(problem.diagnostic_code.clone(), |this, code| {
            this.child(
                Label::new(format!("Rust error {code}"))
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
        })
        .into_any_element()
}

#[cfg(any())]
fn render_conflict_graph(
    graph: &OwnershipConflictGraph,
    show_full_graph: bool,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let nodes = graph.nodes.clone();
    let node_label = |id: &str| {
        graph
            .nodes
            .iter()
            .find(|node| node.id == id)
            .map(|node| node.label.as_str())
            .unwrap_or("value")
    };
    let relation_rows = graph
        .edges
        .iter()
        .map(|edge| {
            format!(
                "{}  ── {} ──▶  {}",
                node_label(&edge.from),
                edge.label,
                node_label(&edge.to)
            )
        })
        .collect::<Vec<_>>();
    let snapshots = graph.snapshots.clone();
    let snapshot_nodes = graph.nodes.clone();
    v_flex()
        .p_3()
        .gap_3()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().status().warning_border)
        .bg(cx.theme().status().warning_background.opacity(0.12))
        .child(
            v_flex()
                .gap_1()
                .child(Label::new("Who borrows what").size(LabelSize::Small))
                .child(
                    Label::new("A reference and the value it points to are separate places. Borrowed means access is restricted—not dead.")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
        )
        .children(relation_rows.into_iter().map(|relation| {
            h_flex()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(Label::new(relation).size(LabelSize::Small).buffer_font(cx))
        }))
        .child(
            Button::new(
                "toggle-conflict-memory-map",
                if show_full_graph {
                    "Hide stack and owner details"
                } else {
                    "Expand memory map"
                },
            )
            .on_click(cx.listener(|panel, _, _window, cx| {
                panel.show_full_conflict_graph = !panel.show_full_conflict_graph;
                cx.notify();
            })),
        )
        .when(show_full_graph, |this| {
            this.child(
                v_flex()
                    .gap_2()
                    .child(Label::new("Reference handles and owner storage").size(LabelSize::Small))
                    .children(nodes.into_iter().map(|node| render_conflict_node(node, cx))),
            )
        })
        .child(Label::new("Permission changes").size(LabelSize::Small))
        .children(
            snapshots
                .into_iter()
                .map(|snapshot| render_conflict_snapshot(snapshot, &snapshot_nodes, cx)),
        )
        .child(provenance_badge(&graph.provenance))
        .when(graph.truncated, |this| {
            this.child(
                Label::new("The conflict map was bounded; expand the advanced compiler sections for remaining facts.")
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
        })
        .into_any_element()
}

#[cfg(any())]
fn render_conflict_node(
    node: OwnershipConflictNode,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let label = node.type_name.as_ref().map_or_else(
        || node.label.clone(),
        |type_name| format!("{}: {type_name}", node.label),
    );
    v_flex()
        .p_2()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(if let Some(range) = node.range {
            Button::new(
                SharedString::from(format!("conflict-node-{}", node.id)),
                label,
            )
            .on_click(cx.listener(move |panel, _, _window, cx| {
                panel.cue_range(range, cx);
            }))
            .into_any_element()
        } else {
            Label::new(label).size(LabelSize::Small).into_any_element()
        })
        .child(
            Label::new(node.role.replace('_', " "))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .child(Label::new(node.memory).size(LabelSize::Small))
        .into_any_element()
}

#[cfg(any())]
fn render_conflict_snapshot(
    snapshot: OwnershipConflictSnapshot,
    nodes: &[OwnershipConflictNode],
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let range = snapshot.range;
    let border = if snapshot.phase == "operation_rejected" {
        cx.theme().status().error_border
    } else {
        cx.theme().colors().border_variant
    };
    v_flex()
        .p_2()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(border)
        .child(
            Button::new(
                SharedString::from(format!("conflict-snapshot-{}", snapshot.phase)),
                snapshot.title,
            )
            .on_click(cx.listener(move |panel, _, _window, cx| {
                panel.cue_range(range, cx);
            })),
        )
        .child(
            Label::new(snapshot.explanation)
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .children(snapshot.states.into_iter().map(|state| {
            let node_label = nodes
                .iter()
                .find(|node| node.id == state.node_id)
                .map(|node| node.label.as_str())
                .unwrap_or("value");
            let color = if state.state.contains("blocked") {
                Color::Warning
            } else if state.state.contains("available") || state.state.contains("ends") {
                Color::Success
            } else {
                Color::Muted
            };
            v_flex()
                .pl_2()
                .child(
                    Label::new(format!("`{node_label}` · {}", state.state))
                        .size(LabelSize::XSmall)
                        .color(color),
                )
                .child(
                    Label::new(state.explanation)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
        }))
        .into_any_element()
}

#[derive(Clone)]
#[cfg(any())]
struct CoreConceptLesson {
    rule: String,
    before: String,
    operation: String,
    after: String,
    usable: String,
    c_risk: String,
    concept_title: String,
    concept: String,
    example: String,
    misconception: String,
    runtime: String,
}

#[cfg(any())]
fn explanation_card(
    title: &'static str,
    body: String,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    v_flex()
        .p_2()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new(title).size(LabelSize::Small))
        .child(Label::new(body).size(LabelSize::Small).color(Color::Muted))
        .into_any_element()
}

#[cfg(any())]
fn core_concept_lesson(category: &str, name: &str) -> CoreConceptLesson {
    match category {
        "use_after_move" | "partial_move" => CoreConceptLesson {
            rule: "A non-Copy Rust value has one owner; moving it transfers the right to use and eventually destroy it.".to_owned(),
            before: format!("{name}: owns value ●"),
            operation: if category == "partial_move" { "move one field".to_owned() } else { "move value".to_owned() },
            after: if category == "partial_move" { format!("{name}: partially usable ◐") } else { format!("{name}: unavailable →") },
            usable: if category == "partial_move" { "Unmoved fields remain usable; the complete struct does not.".to_owned() } else { "Only the new owner is usable. Borrow instead when ownership should stay here.".to_owned() },
            c_risk: "The closest C mistake is copying an owning pointer without defining which copy calls free. That can produce use-after-free, double-free, or a leak. Rust records the transfer statically.".to_owned(),
            concept_title: "Ownership and moves".to_owned(),
            concept: "A move usually copies the small representation (for String: pointer, length, capacity) and invalidates the old Rust place. It does not deep-copy the heap allocation.".to_owned(),
            example: "let text = String::from(\"hello\");\nlet view = &text;       // borrow: text stays owner\nprintln!(\"{view}\");".to_owned(),
            misconception: "a Rust move always moves heap bytes. Usually only the owner handle changes.".to_owned(),
            runtime: "Moves and ordinary borrows have no reference-count or borrow-flag cost. Clone may allocate and copy. Rc/Arc add counters and shared-allocation lifetime management.".to_owned(),
        },
        "immutable_mutation" => CoreConceptLesson {
            rule: "Mutation needs an explicit mutable path, or a type that enforces mutation rules at runtime.".to_owned(),
            before: format!("{name}: shared/read-only"),
            operation: "write".to_owned(),
            after: "rejected: no mutable permission".to_owned(),
            usable: "Use &mut when exclusive access is available; use RefCell only for single-threaded runtime-checked interior mutability.".to_owned(),
            c_risk: "C permits writes through aliased pointers unless const is used consistently. A hidden writer can invalidate readers or iterators; across threads it can become a data race.".to_owned(),
            concept_title: "Mutability and interior mutability".to_owned(),
            concept: "&mut proves exclusive access at compile time. RefCell<T> keeps an immutable outer owner but checks the many-readers-or-one-writer rule with a runtime borrow flag.".to_owned(),
            example: "let value = std::cell::RefCell::new(String::new());\nvalue.borrow_mut().push('!');\nprintln!(\"{}\", value.borrow());".to_owned(),
            misconception: "RefCell makes aliasing unrestricted. It enforces the same rule and panics when overlapping borrows violate it.".to_owned(),
            runtime: "&mut is compile-time-only and normally zero-cost. RefCell stores a borrow flag; borrow()/borrow_mut() update and check it and may panic. It is not thread-safe.".to_owned(),
        },
        "multiple_mutable_borrows" | "mutable_while_shared" | "move_while_borrowed" | "assign_while_borrowed" => CoreConceptLesson {
            rule: "For one value, Rust permits many live shared references or one live mutable reference, and an owner cannot move or replace the value while those references are needed.".to_owned(),
            before: format!("{name}: borrowed ◇"),
            operation: if category == "move_while_borrowed" { "move owner".to_owned() } else { "request write/exclusive access".to_owned() },
            after: "rejected while earlier borrow is live".to_owned(),
            usable: "The earlier reference remains usable. End its last use sooner, narrow its scope, or perform the mutation before creating it.".to_owned(),
            c_risk: "In C, reallocating, freeing, or mutating through one pointer while another pointer remains live can leave a dangling pointer, invalidate an iterator, or expose inconsistent data.".to_owned(),
            concept_title: "Aliasing, exclusivity, and non-lexical lifetimes".to_owned(),
            concept: "A borrow remains live until its last use on each control-flow path, not necessarily until the closing brace. Rust calls this non-lexical lifetime analysis.".to_owned(),
            example: "let mut text = String::from(\"hi\");\nlet view = &text;\nprintln!(\"{view}\"); // last use of view\ntext.push('!');         // accepted".to_owned(),
            misconception: "every reference lasts until the end of its lexical block. Modern Rust normally ends it after the last required use.".to_owned(),
            runtime: "Ordinary references and NLL checks are compile-time-only. Rc<RefCell<T>> adds non-atomic reference counts plus runtime borrow checks; Arc<Mutex<T>> adds atomic counts and locking.".to_owned(),
        },
        _ => CoreConceptLesson {
            rule: "Ownership decides who destroys a value; borrowing grants temporary access without transferring that responsibility.".to_owned(),
            before: format!("{name}: available ●"),
            operation: "ownership operation".to_owned(),
            after: "compiler checks the resulting owner and live references".to_owned(),
            usable: "Follow the compiler-exact timeline below to see which place remains available.".to_owned(),
            c_risk: "C leaves pointer lifetime, aliasing, and free responsibility to conventions. Rust makes those responsibilities part of the checked program.".to_owned(),
            concept_title: "Ownership and borrowing".to_owned(),
            concept: "Moves transfer destruction responsibility; borrows create temporary non-owning views; Drop runs when the final owner leaves scope.".to_owned(),
            example: "fn read(value: &String) { println!(\"{value}\"); }\nlet value = String::from(\"data\");\nread(&value);\nprintln!(\"{value}\");".to_owned(),
            misconception: "the borrow checker manages memory at runtime. Ordinary ownership and borrow checks are compile-time analysis.".to_owned(),
            runtime: "Plain ownership and references normally add no runtime bookkeeping. Wrapper types add only the mechanisms their semantics require.".to_owned(),
        },
    }
}

fn problem_story(category: &str, name: &str) -> (String, String, String) {
    match category {
        "use_after_move" => (
            format!("`{name}` was used after its value moved"),
            format!("An earlier operation transferred ownership out of `{name}`. The old name no longer owns a value."),
            "Allowing the old name to be used could make two places believe they uniquely own the same resource.".to_owned(),
        ),
        "partial_move" => (
            format!("Part of `{name}` moved"),
            format!("One non-Copy field was taken out of `{name}`. Other fields may still be usable, but the whole value is incomplete."),
            "Rust prevents using the complete struct until the moved field is put back.".to_owned(),
        ),
        "multiple_mutable_borrows" => (
            format!("`{name}` has two overlapping mutable borrows"),
            "A mutable reference is still live when another mutable reference is requested.".to_owned(),
            "Rust permits either many readers or one writer at a time, never two writers to the same value.".to_owned(),
        ),
        "mutable_while_shared" => (
            format!("`{name}` is borrowed for reading and writing at the same time"),
            "A shared reference remains live when code tries to create a mutable reference.".to_owned(),
            "Changing a value while another reference may read it would invalidate the reader's assumptions.".to_owned(),
        ),
        "move_while_borrowed" => (
            format!("`{name}` moved while a borrow was still live"),
            "A reference still points at the value when ownership is transferred elsewhere.".to_owned(),
            "The move could invalidate the existing reference, so Rust requires the borrow to end first.".to_owned(),
        ),
        "assign_while_borrowed" => (
            format!("`{name}` changed while it was borrowed"),
            "Code assigns a new value while a reference to the old value remains live.".to_owned(),
            "Rust keeps references stable for their complete live range.".to_owned(),
        ),
        "use_while_mutably_borrowed" => (
            format!("`{name}` was used through its owner while an exclusive borrow was live"),
            "An `&mut` reference temporarily holds the only permitted access path to the overlapping value.".to_owned(),
            "Using the owner at the same time would break the mutable reference's exclusivity promise.".to_owned(),
        ),
        "move_out_of_borrowed_content" => (
            format!("Code tried to move owned data out through `{name}`"),
            "A borrowed container or reference does not own the value it points into, so it cannot transfer that value's drop responsibility.".to_owned(),
            "Moving out would leave the real owner with an incomplete value it still expects to own.".to_owned(),
        ),
        "immutable_mutation" => (
            format!("`{name}` cannot be changed through this path"),
            "The current binding or reference does not grant mutable access.".to_owned(),
            "Mutation must be explicit, exclusive, or managed by a runtime-checked interior-mutability type.".to_owned(),
        ),
        "missing_lifetime" => (
            "Rust needs to know which input keeps this returned reference valid".to_owned(),
            format!("The reference involving `{name}` has no unambiguous lifetime relationship to an input."),
            "A returned reference must never remain usable after the value it points into is gone.".to_owned(),
        ),
        "returning_local_reference" => (
            "A reference points into data that will be dropped before the caller can use it".to_owned(),
            format!("`{name}` refers to storage owned inside the current function."),
            "Function-local owners are cleaned up on return, so references into them would dangle.".to_owned(),
        ),
        "borrowed_value_too_short" | "temporary_dropped_while_borrowed" => (
            format!("The owner of `{name}` does not live long enough"),
            "The program keeps a reference beyond the point where its owner or temporary is dropped.".to_owned(),
            "Every use of a reference must occur while its referent is still alive.".to_owned(),
        ),
        "trait_requirement" => (
            format!("The type involving `{name}` does not satisfy a required trait"),
            "The selected API or generic context requires a capability the concrete type does not provide.".to_owned(),
            "Rust must prove every declared trait capability before generating the call or crossing the boundary.".to_owned(),
        ),
        "type_mismatch" => (
            format!("The type produced for `{name}` differs from the type this context expects"),
            "The producer and consumer currently describe different representations or ownership contracts.".to_owned(),
            "Conversions and ownership changes stay explicit so allocation, failure, and lifetime behavior cannot be hidden.".to_owned(),
        ),
        "method_or_trait_unavailable" => (
            format!("The requested method is not available for `{name}` through this receiver path"),
            "Method lookup could not find a compatible inherent method or in-scope trait method for the resolved receiver type.".to_owned(),
            "The compiler must identify one function and prove that the receiver supplies its ownership and mutability contract.".to_owned(),
        ),
        "closure_may_outlive_borrow" | "borrowed_data_escapes" => (
            format!("A closure or stored reference may outlive `{name}`"),
            "Borrowed data is captured or passed into a context that can keep it beyond the current stack region.".to_owned(),
            "The captured environment must remain valid for every later closure or callback execution.".to_owned(),
        ),
        "await_outside_async" => (
            "`.await` appears outside an async context".to_owned(),
            "Suspending requires a future state machine and an async caller that can poll it.".to_owned(),
            "Ordinary synchronous functions do not have a suspended state in which locals can be preserved.".to_owned(),
        ),
        "recursive_async_function" => (
            "Recursive async calls need an indirection boundary".to_owned(),
            "The future would otherwise contain another instance of itself with no finite compile-time size.".to_owned(),
            "Boxing or restructuring introduces a finite handle to the recursive future state.".to_owned(),
        ),
        _ => (
            format!("Rust rejected an operation involving `{name}`"),
            "The compiler found a type, lifetime, trait, ownership, or borrowing contract that this operation would violate.".to_owned(),
            "Use the visual timeline and compiler message to identify the two contracts that disagree before choosing a repair.".to_owned(),
        ),
    }
}

#[cfg(any())]
fn render_coach_flow(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
    show_c_comparison: bool,
    show_all_events: bool,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    if model.events.is_empty() {
        return empty_card(
            "Compiler ownership events are still loading. Save the file or press Refresh.",
        );
    }
    let name = problem
        .map(|problem| problem.binding_name.as_str())
        .or(model.selected_place.as_deref())
        .unwrap_or("value");
    let event_limit = if show_all_events { 96 } else { 8 };
    let mut root = v_flex()
        .gap_3()
        .child(Label::new(format!("The ownership story for `{name}`")).size(LabelSize::Large))
        .child(
            h_flex()
                .gap_2()
                .child(flow_legend("●", "usable"))
                .child(flow_legend("◇", "shared borrow"))
                .child(flow_legend("◆", "mutable borrow"))
                .child(flow_legend("→", "moved"))
                .child(flow_legend("×", "dropped")),
        )
        .children(model.events.clone().into_iter().take(event_limit).map(|event| {
            let range = event.range;
            let line = event.range.start.line + 1;
            v_flex()
                .p_3()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    Button::new(
                        SharedString::from(format!("coach-flow-{}", event.event_id)),
                        format!(
                            "{}  line {line}: {}",
                            state_symbol(&event.state),
                            event.kind.replace('_', " ")
                        ),
                    )
                    .on_click(cx.listener(move |panel, _, _window, cx| {
                        panel.cue_range(range, cx);
                    })),
                )
                .child(
                    Label::new(guided_event_explanation(&event.kind, &event.place))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
        }))
        .when(model.events.len() > event_limit, |this| {
            this.child(
                Label::new(format!(
                    "{} more compiler events are hidden. Use Show complete flow when you need the full trace.",
                    model.events.len() - event_limit
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            )
        });
    if show_c_comparison {
        root = root.child(render_conceptual_c_sketch(model, false, cx));
    }
    root.into_any_element()
}

#[cfg(any())]
fn flow_legend(symbol: &'static str, label: &'static str) -> AnyElement {
    h_flex()
        .gap_1()
        .child(Label::new(symbol))
        .child(
            Label::new(label)
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .into_any_element()
}

#[cfg(any())]
fn render_coach_intent(answers: IntentAnswers, cx: &mut Context<RustWorkbenchPanel>) -> AnyElement {
    let recommendation = intent_recommendation(answers);
    v_flex()
        .gap_3()
        .child(Label::new("Choose the behavior you actually need").size(LabelSize::Large))
        .child(
            Label::new("These answers rank alternatives; they do not silently change your program's semantics.")
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .child(intent_question(
            "Should several owners see the same value?",
            IntentQuestion::MultipleOwners,
            answers.multiple_owners,
            cx,
        ))
        .child(intent_question(
            "Must the shared value be mutated?",
            IntentQuestion::Mutation,
            answers.mutation,
            cx,
        ))
        .child(intent_question(
            "Will ownership cross a thread boundary?",
            IntentQuestion::CrossesThreads,
            answers.crosses_threads,
            cx,
        ))
        .child(intent_question(
            "Would independent copied data be correct?",
            IntentQuestion::IndependentClone,
            answers.independent_clone,
            cx,
        ))
        .child(
            v_flex()
                .p_3()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(Label::new("Current recommendation").size(LabelSize::Small))
                .child(Label::new(recommendation).size(LabelSize::Small).color(Color::Muted)),
        )
        .into_any_element()
}

#[cfg(any())]
fn intent_question(
    prompt: &'static str,
    question: IntentQuestion,
    answer: Option<bool>,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    h_flex()
        .p_2()
        .gap_2()
        .justify_between()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new(prompt).size(LabelSize::Small))
        .child(
            h_flex()
                .gap_1()
                .child(
                    Button::new(
                        SharedString::from(format!("intent-{question:?}-yes")),
                        if answer == Some(true) {
                            "● Yes"
                        } else {
                            "Yes"
                        },
                    )
                    .on_click(cx.listener(move |panel, _, _window, cx| {
                        panel.intent_answers.set(question, true);
                        cx.notify();
                    })),
                )
                .child(
                    Button::new(
                        SharedString::from(format!("intent-{question:?}-no")),
                        if answer == Some(false) {
                            "● No"
                        } else {
                            "No"
                        },
                    )
                    .on_click(cx.listener(move |panel, _, _window, cx| {
                        panel.intent_answers.set(question, false);
                        cx.notify();
                    })),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
fn intent_recommendation(answers: IntentAnswers) -> &'static str {
    match (
        answers.crosses_threads,
        answers.multiple_owners,
        answers.mutation,
        answers.independent_clone,
    ) {
        (_, _, _, Some(true)) => {
            "Clone the data when independent values are the intended behavior."
        }
        (Some(true), _, Some(true), _) => {
            "Use Arc<Mutex<T>> for shared cross-thread mutation, or Arc<RwLock<T>> for read-heavy access."
        }
        (Some(true), _, _, _) => "Use Arc<T> for shared ownership across threads.",
        (_, Some(true), Some(true), _) => {
            "Use Rc<RefCell<T>> for shared mutation confined to one thread; runtime borrow checks may panic."
        }
        (_, Some(true), _, _) => "Use Rc<T> for shared ownership confined to one thread.",
        (_, _, Some(true), _) => {
            "Prefer an ordinary mutable borrow. Use RefCell<T> only when static borrowing cannot express the design."
        }
        _ => {
            "Borrow the value when another scope only needs temporary access; move it when ownership should transfer."
        }
    }
}

fn render_guided_repairs(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
    preview_repair_id: Option<&str>,
    show_alternatives: bool,
    exact_mode: bool,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let ideas = problem
        .map(|problem| learning_catalog::repair_ideas(&problem.category))
        .unwrap_or_default();
    let preferred_repairs = model
        .repairs
        .iter()
        .filter(|repair| repair.strategy == "language_fix")
        .take(1)
        .cloned()
        .collect::<Vec<_>>();
    let alternative_repairs = model
        .repairs
        .iter()
        .filter(|repair| repair.strategy != "language_fix")
        .cloned()
        .collect::<Vec<_>>();
    let preferred = preferred_repairs
        .is_empty()
        .then(|| problem.and_then(|problem| preferred_mutability_repair(problem, model)))
        .flatten();
    let has_preferred = !preferred_repairs.is_empty() || preferred.is_some();
    let has_alternatives = !alternative_repairs.is_empty() || ideas.len() > 1;
    v_flex()
        .p_3()
        .gap_3()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new("Fix it safely").size(LabelSize::Large))
        .child(
            Label::new(if has_preferred {
                "Start with ordinary compile-time mutability. Shared-ownership wrappers are design alternatives, not automatic upgrades."
            } else if model.repairs.is_empty() {
                "Start with the most direct design change. Automatic editing stays unavailable until the complete rewrite is compiler-validated."
            } else {
                "Preview a diff first. Apply remains unavailable until rustc accepts the complete rewrite."
            })
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .when(!preferred_repairs.is_empty(), |this| {
            this.child(render_repairs(
                preferred_repairs,
                model.source_hash.clone(),
                preview_repair_id,
                exact_mode,
                cx,
            ))
        })
        .when_some(preferred, |this, preferred| {
            this.child(
                v_flex()
                    .p_2()
                    .gap_1()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().status().success)
                    .child(Label::new(format!("Best first · {}", preferred.title)).size(LabelSize::Small))
                    .child(Label::new(preferred.diff).size(LabelSize::Small).buffer_font(cx))
                    .child(Label::new(preferred.impact).size(LabelSize::XSmall))
                    .child(
                        Label::new("Apply remains locked until an isolated Cargo check removes this diagnostic and introduces no new errors.")
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
        })
        .when(!has_preferred && model.repairs.is_empty(), |this| {
            this.children(ideas.iter().take(1).enumerate().map(|(index, idea)| {
                v_flex()
                    .p_2()
                    .gap_1()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        Label::new(format!("{} · {}", index + 1, idea.title))
                            .size(LabelSize::Small),
                    )
                    .child(Label::new(idea.intent).size(LabelSize::XSmall))
                    .child(
                        Label::new(format!("Trade-off: {}", idea.tradeoff))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
            }))
        })
        .when(has_alternatives, |this| {
            this.child(
                Button::new(
                    "toggle-repair-alternatives",
                    if show_alternatives {
                        "Hide other designs"
                    } else {
                        "Other designs"
                    },
                )
                .on_click(cx.listener(|panel, _, _window, cx| {
                    panel.show_repair_alternatives = !panel.show_repair_alternatives;
                    cx.notify();
                })),
            )
        })
        .when(show_alternatives && !alternative_repairs.is_empty(), |this| {
            this.child(render_repairs(
                alternative_repairs,
                model.source_hash.clone(),
                preview_repair_id,
                exact_mode,
                cx,
            ))
        })
        .when(show_alternatives, |this| {
            this.children(ideas.iter().skip(1).take(3).enumerate().map(
                |(index, idea)| {
                    v_flex()
                        .p_2()
                        .gap_1()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().colors().border_variant)
                        .child(
                            Label::new(format!("Alternative {} · {}", index + 1, idea.title))
                                .size(LabelSize::Small),
                        )
                        .child(Label::new(idea.intent).size(LabelSize::XSmall))
                        .child(
                            Label::new(format!("Trade-off: {}", idea.tradeoff))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                },
            ))
        })
        .into_any_element()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreferredRepairSummary {
    title: String,
    diff: String,
    impact: String,
}

fn preferred_mutability_repair(
    problem: &OwnershipProblem,
    model: &OwnershipModel,
) -> Option<PreferredRepairSummary> {
    if problem.category != "immutable_mutation" {
        return None;
    }
    let requirement = model.mutation_requirement.as_ref()?;
    if requirement.access_source == "&self" {
        let function = model
            .source_context
            .as_ref()
            .and_then(|context| {
                context
                    .breadcrumbs
                    .iter()
                    .find(|item| item.kind == "function")
            })
            .map(|item| item.label.as_str())
            .unwrap_or("method");
        return Some(PreferredRepairSummary {
            title: "give the method exclusive mutable access".to_owned(),
            diff: format!(
                "- fn {function}(&self, …)\n+ fn {function}(&mut self, …)"
            ),
            impact: "Callers must temporarily hold mutable access to the owner. This adds no runtime borrow checks, reference counts, or locks."
                .to_owned(),
        });
    }
    if let Some(binding) = requirement.access_source.strip_prefix("immutable binding ") {
        return Some(PreferredRepairSummary {
            title: format!("declare `{binding}` mutable"),
            diff: format!("- let {binding} = …;\n+ let mut {binding} = …;"),
            impact: "The binding stays uniquely owned; this only permits mutation through that local name."
                .to_owned(),
        });
    }
    None
}

fn render_coach_result(
    problem: Option<&OwnershipProblem>,
    problems: &OwnershipProblems,
    model: &OwnershipModel,
    verification: Option<&RepairVerification>,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    if let Some(verification) = verification {
        let target = format!(
            "{} for `{}`",
            verification
                .diagnostic_code
                .as_deref()
                .unwrap_or(verification.category.as_str()),
            verification.binding_name
        );
        match &verification.state {
            RepairVerificationState::Applying => {
                return verification_card(
                    "Applying the selected rewrite…",
                    format!("Editing {target} with `{}`.", verification.repair_title),
                    Color::Info,
                    cx,
                );
            }
            RepairVerificationState::Checking => {
                return verification_card(
                    "Checking this selected issue…",
                    format!(
                        "The edit was applied. Waiting for a fresh Cargo diagnostic for {target}."
                    ),
                    Color::Info,
                    cx,
                );
            }
            RepairVerificationState::Resolved {
                remaining_file_problems,
            } => {
                return verification_card(
                    "✓ This selected Rust issue is resolved",
                    if *remaining_file_problems == 0 {
                        format!(
                            "Cargo no longer reports {target}. No other tracked learning diagnostics remain in this file. Runtime behavior still needs tests."
                        )
                    } else {
                        format!(
                            "Cargo no longer reports {target}. {remaining_file_problems} other tracked Rust problem(s) remain; use Next to inspect them. Runtime behavior still needs tests."
                        )
                    },
                    Color::Success,
                    cx,
                );
            }
            RepairVerificationState::IntroducedProblems { summaries } => {
                return verification_card(
                    "The repair introduced new compiler problems",
                    format!(
                        "The selected diagnostic disappeared, but Apply is not considered successful because these new diagnostics appeared:\n• {}\nUse Undo, then choose a different repair or update the affected callers.",
                        summaries.join("\n• ")
                    ),
                    Color::Error,
                    cx,
                );
            }
            RepairVerificationState::StillPresent { current_line } => {
                return verification_card(
                    "The selected issue is still present",
                    format!(
                        "Cargo still reports {target} near line {} after `{}`. Undo or compare another validated rewrite.",
                        current_line + 1,
                        verification.repair_title
                    ),
                    Color::Error,
                    cx,
                );
            }
            RepairVerificationState::Failed(message) => {
                return verification_card(
                    "The repair could not be verified",
                    message.to_string(),
                    Color::Error,
                    cx,
                );
            }
        }
    }
    if problems.problems.is_empty() && problems.status == "ready" {
        return v_flex()
            .p_3()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().status().success)
            .child(Label::new("✓ Cargo check reports no tracked learning diagnostic").size(LabelSize::Large))
            .child(
                Label::new("The current source hash matches the analyzer result. Run the program or its tests to validate behavior as well as type safety.")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .into_any_element();
    }
    let Some(problem) = problem else {
        return empty_card("Waiting for a fresh Cargo check result for the current source.");
    };
    let primary_range = problem.primary_range;
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().status().error)
        .child(Label::new("The selected Rust problem is still present").size(LabelSize::Large))
        .child(
            Label::new(format!(
                "Rust still rejects `{}`. Review the alternatives, apply one, then wait for Cargo check.",
                problem.binding_name
            ))
            .size(LabelSize::Small)
            .color(Color::Muted),
        )
        .child(
            Button::new("result-show-error", "Show remaining error").on_click(
                cx.listener(move |panel, _, _window, cx| panel.cue_range(primary_range, cx)),
            ),
        )
        .when(!model.repairs.is_empty(), |this| {
            this.child(
                Label::new(format!("{} validated alternatives are available.", model.repairs.len()))
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
        })
        .into_any_element()
}

fn verification_card(
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    color: Color,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(match color {
            Color::Success => cx.theme().status().success,
            Color::Error => cx.theme().status().error,
            _ => cx.theme().colors().border_variant,
        })
        .child(Label::new(title).size(LabelSize::Large).color(color))
        .child(
            Label::new(detail)
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .into_any_element()
}

fn provenance_badge(label: &str) -> impl IntoElement {
    Label::new(format!("[{}]", label.replace('_', " ")))
        .size(LabelSize::XSmall)
        .color(Color::Muted)
}

fn empty_card(message: impl Into<SharedString>) -> AnyElement {
    v_flex()
        .p_3()
        .rounded_md()
        .border_1()
        .child(
            Label::new(message)
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .into_any_element()
}

fn render_timeline(
    model: &OwnershipModel,
    exact_mode: bool,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    if model.events.is_empty() {
        return empty_card("No ownership events yet. Save the file and wait for Cargo check.");
    }
    v_flex()
        .gap_2()
        .child(provenance_badge(&model.precision))
        .children(model.events.clone().into_iter().take(96).map(|event| {
            let range = event.range;
            let loan = event.loan_id.map(|loan| format!(" · loan {loan}"));
            let title = format!(
                "{}  {} · {}{}",
                state_symbol(&event.state),
                event.kind.replace('_', " "),
                event.place,
                loan.as_deref().unwrap_or_default(),
            );
            v_flex()
                .p_2()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    Button::new(
                        SharedString::from(format!("timeline-{}", event.event_id)),
                        title,
                    )
                    .on_click(cx.listener(move |panel, _, _window, cx| {
                        panel.cue_range(range, cx);
                    })),
                )
                .child(
                    Label::new(guided_event_explanation(&event.kind, &event.place))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .when(exact_mode, |this| {
                    this.child(
                        Label::new(format!(
                            "state {} · body {:016x} · MIR bb{}[{}]",
                            event.state, event.body_id, event.basic_block, event.statement_index,
                        ))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                    )
                })
                .when_some(event.detail, |this, detail| {
                    this.child(
                        Label::new(detail)
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                })
        }))
        .when(model.events.len() > 96, |this| {
            this.child(
                Label::new(format!(
                    "{} additional events hidden",
                    model.events.len() - 96
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            )
        })
        .into_any_element()
}

fn render_lifetimes(
    model: &OwnershipModel,
    exact_mode: bool,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    if model.loans.is_empty() {
        return empty_card(
            "No active loan model for this binding. Select a value that is borrowed.",
        );
    }
    v_flex()
        .gap_3()
        .child(provenance_badge("compiler_exact"))
        .children(model.loans.clone().into_iter().take(24).map(|loan| {
            let reserve = loan.reserve.range;
            let end_count = loan.end_points.len();
            v_flex()
                .p_3()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    Button::new(
                        SharedString::from(format!("loan-{}-reserve", loan.loan_id)),
                        format!(
                            "{} loan {} of {}",
                            loan_symbol(&loan.kind),
                            loan.loan_id,
                            loan.place
                        ),
                    )
                    .on_click(cx.listener(move |panel, _, _window, cx| {
                        panel.cue_range(reserve, cx);
                    })),
                )
                .child(
                    Label::new("reserve ●━━━━━━━━ live loan ━━━━━━━━■ end").size(LabelSize::Small),
                )
                .child(
                    Label::new(format!(
                        "The {} borrow prevents incompatible access until its final live point.",
                        loan.kind,
                    ))
                    .size(LabelSize::Small)
                    .color(Color::Muted),
                )
                .children(
                    loan.end_points
                        .into_iter()
                        .take(16)
                        .enumerate()
                        .map(|(index, point)| {
                            loan_point_button("end", loan.loan_id, index, point, cx)
                        }),
                )
                .when(exact_mode, |this| {
                    this.child(
                        Label::new(format!(
                            "{} live MIR points · {end_count} CFG endpoints{}",
                            loan.live_points.len(),
                            if loan.truncated {
                                " · display truncated"
                            } else {
                                ""
                            },
                        ))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                    )
                })
        }))
        .when(model.loans.len() > 24, |this| {
            this.child(
                Label::new(format!(
                    "{} additional loans hidden",
                    model.loans.len() - 24
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            )
        })
        .into_any_element()
}

fn loan_point_button(
    kind: &'static str,
    loan_id: u32,
    index: usize,
    point: OwnershipLoanPoint,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let range = point.range;
    Button::new(
        SharedString::from(format!("loan-{loan_id}-{kind}-{index}")),
        format!(
            "{kind} at MIR bb{}[{}]",
            point.basic_block, point.statement_index
        ),
    )
    .on_click(cx.listener(move |panel, _, _window, cx| panel.cue_range(range, cx)))
    .into_any_element()
}

fn render_memory(
    model: &OwnershipModel,
    exact_mode: bool,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    if model.bindings.is_empty() {
        return empty_card(
            "No compiler memory model for this cursor yet. Select the binding name after Cargo check.",
        );
    }
    v_flex()
        .gap_3()
        .child(
            Label::new("Source-level ownership topology")
                .size(LabelSize::Large),
        )
        .child(
            Label::new("Stack/heap labels describe ownership semantics; optimized machine placement may differ.")
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .children(model.bindings.clone().into_iter().take(24).map(|binding| {
            render_binding_memory(binding, exact_mode, cx)
        }))
        .when(model.bindings.len() > 24, |this| {
            this.child(
                Label::new(format!(
                    "{} additional bindings hidden",
                    model.bindings.len() - 24
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            )
        })
        .into_any_element()
}

fn render_binding_memory(
    binding: OwnershipBinding,
    exact_mode: bool,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let range = binding.range;
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(
            Button::new(
                SharedString::from(format!("memory-binding-{}", binding.id)),
                format!("{}: {}", binding.name, binding.type_name),
            )
            .on_click(cx.listener(move |panel, _, _window, cx| panel.cue_range(range, cx))),
        )
        .when(exact_mode, |this| {
            this.child(
                Label::new(format!(
                    "target layout: size {} · align {}",
                    binding
                        .size
                        .map_or_else(|| "unknown".to_owned(), |size| format!("{size} B")),
                    binding
                        .align
                        .map_or_else(|| "unknown".to_owned(), |align| format!("{align} B")),
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            )
        })
        .children(
            binding
                .memory_layers
                .into_iter()
                .enumerate()
                .flat_map(|(index, layer)| {
                    let arrow = (index > 0).then(|| {
                        Label::new("                 ↓ owns / contains")
                            .size(LabelSize::XSmall)
                            .color(Color::Muted)
                            .into_any_element()
                    });
                    let card = h_flex()
                        .p_2()
                        .gap_2()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().colors().border_variant)
                        .child(Label::new(memory_symbol(&layer.storage)).size(LabelSize::Large))
                        .child(
                            v_flex()
                                .child(
                                    Label::new(format!("{} · {}", layer.storage, layer.label))
                                        .size(LabelSize::Small),
                                )
                                .child(
                                    Label::new(if exact_mode {
                                        format!("{} · {}", layer.type_name, layer.provenance)
                                    } else {
                                        beginner_memory_explanation(&layer.kind).to_owned()
                                    })
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                                ),
                        )
                        .into_any_element();
                    arrow.into_iter().chain(std::iter::once(card))
                }),
        )
        .into_any_element()
}

fn render_repairs(
    repairs: Vec<rust_analyzer_ext::OwnershipRepair>,
    source_hash: String,
    preview_repair_id: Option<&str>,
    exact_mode: bool,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    if repairs.is_empty() {
        return empty_card("No ownership repair alternative is available at this cursor.");
    }
    let has_candidates = repairs
        .iter()
        .any(|repair| !repair_is_compiler_validated(repair));
    v_flex()
        .gap_2()
        .child(provenance_badge(if has_candidates {
            "IDE candidate · rustc validation required"
        } else {
            "compiler_validated"
        }))
        .children(repairs.into_iter().map(|repair| {
            let validated = repair_is_compiler_validated(&repair);
            let title = repair.title.clone();
            let repair_id = repair.id.clone();
            let source_hash = source_hash.clone();
            let is_previewed = preview_repair_id == Some(repair.id.as_str());
            let preview_id = repair.id.clone();
            v_flex()
                .p_3()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(Label::new(repair.title.clone()).size(LabelSize::Small))
                .child(
                    Label::new(if validated {
                        "rustc accepted this complete rewrite; Apply is enabled."
                    } else {
                        "Design candidate only. Preview starts rustc; Apply stays hidden until it passes."
                    })
                    .size(LabelSize::XSmall)
                    .color(if validated { Color::Success } else { Color::Warning }),
                )
                .child(
                    Label::new(repair.semantics.clone())
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new(
                                SharedString::from(format!("preview-{}", repair.id)),
                                if is_previewed {
                                    "Hide semantic preview"
                                } else if validated {
                                    "Preview validated result"
                                } else {
                                    "Preview & compiler-check"
                                },
                            )
                            .on_click(cx.listener(
                                move |panel, _, _window, cx| {
                                    panel.preview_repair(preview_id.clone(), cx);
                                },
                            )),
                        )
                        .when(validated, |this| this.child(
                            Button::new(
                                SharedString::from(format!("apply-{}", repair.id)),
                                "Apply (undoable)",
                            )
                            .on_click(cx.listener(
                                move |panel, _, window, cx| {
                                    panel.apply_repair(
                                        repair_id.clone(),
                                        title.clone(),
                                        source_hash.clone(),
                                        window,
                                        cx,
                                    )
                                },
                            )),
                        )),
                )
                .when(is_previewed, |this| {
                    this.child(
                        Label::new(repair.diff.clone())
                            .size(LabelSize::XSmall)
                            .buffer_font(cx),
                    )
                    .child(render_repair_counterfactual(&repair, cx))
                    .child(
                        Label::new(format!(
                            "Trade-offs · ownership: {} · mutation: {} · runtime: {}",
                            repair.effects.ownership,
                            repair.effects.mutation,
                            repair.effects.runtime_risk
                        ))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                    )
                    .when(exact_mode, |this| {
                        this.child(
                            Label::new(format!(
                                "Threads: {} · cost: {}",
                                repair.effects.thread_safety, repair.effects.cost
                            ))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                        )
                    })
                })
        }))
        .into_any_element()
}

fn render_repair_counterfactual(
    repair: &rust_analyzer_ext::OwnershipRepair,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let topology = match repair.strategy.as_str() {
        "rc" => {
            "stack owner A ─► Rc allocation ◄─ stack owner B\nshared value · non-atomic strong count"
        }
        "arc" => {
            "thread owner A ─► Arc allocation ◄─ thread owner B\nshared value · atomic strong count"
        }
        "refcell" => {
            "stack owner ─► RefCell { borrow flag | value }\nshared outer access · runtime checked mutation"
        }
        "rc_refcell" => {
            "owner A ─► Rc { count | RefCell { flag | value } } ◄─ owner B\nshared ownership · runtime checked mutation"
        }
        "arc_mutex" => {
            "thread A ─► Arc { atomic count | Mutex { lock | value } } ◄─ thread B\nshared ownership · blocking exclusive mutation"
        }
        _ => {
            "The source diff changes ownership or access as described below; no wrapper topology is assumed."
        }
    };
    v_flex()
        .p_2()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().status().info)
        .bg(cx.theme().status().info_background.opacity(0.1))
        .child(Label::new("Counterfactual result — source is not changed yet").size(LabelSize::Small))
        .child(Label::new(topology).size(LabelSize::XSmall).buffer_font(cx))
        .child(Label::new(format!("Ownership: {}", repair.effects.ownership)).size(LabelSize::XSmall))
        .child(Label::new(format!("Mutation: {}", repair.effects.mutation)).size(LabelSize::XSmall))
        .child(Label::new(format!("Runtime risk: {}", repair.effects.runtime_risk)).size(LabelSize::XSmall))
        .child(Label::new(format!("Cost: {}", repair.effects.cost)).size(LabelSize::XSmall))
        .child(
            Label::new("Only Apply edits the source; the compiler then independently verifies the selected diagnostic.")
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .into_any_element()
}

fn render_conceptual_c_sketch(
    model: &OwnershipModel,
    exact_mode: bool,
    _cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let Some(sketch) = &model.c_sketch else {
        return empty_card("Select a binding to generate its deterministic C-like intent sketch.");
    };
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .child(Label::new(sketch.title.clone()).size(LabelSize::Large))
        .child(provenance_badge(&sketch.provenance))
        .child(
            Label::new(sketch.code.clone())
                .size(LabelSize::Small)
                .buffer_font(_cx),
        )
        .child(
            Label::new(sketch.warning.clone())
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .when(exact_mode, |this| {
            this.child(
                Label::new(format!(
                    "Linked to {} compiler ownership events",
                    sketch.linked_event_ids.len()
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            )
        })
        .into_any_element()
}

fn render_c_view(
    model: &OwnershipModel,
    exact_mode: bool,
    mode: CViewMode,
    state: CGenerationState,
    artifact: Option<GeneratedCArtifact>,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .gap_1()
                .child(
                    Button::new(
                        "c-view-conceptual",
                        selected_button_label(mode == CViewMode::Conceptual, "Conceptual"),
                    )
                    .on_click(cx.listener(|panel, _, _window, cx| {
                        panel.c_view_mode = CViewMode::Conceptual;
                        panel.cancel_generated_c(cx);
                    })),
                )
                .child(
                    Button::new(
                        "c-view-generated",
                        selected_button_label(mode == CViewMode::Generated, "Generated C"),
                    )
                    .on_click(cx.listener(|panel, _, _window, cx| {
                        panel.c_view_mode = CViewMode::Generated;
                        panel.schedule_generated_c(cx);
                    })),
                ),
        )
        .child(if mode == CViewMode::Conceptual {
            render_conceptual_c_sketch(model, exact_mode, cx)
        } else {
            render_generated_c(state, artifact, cx)
        })
        .into_any_element()
}

fn render_generated_c(
    state: CGenerationState,
    artifact: Option<GeneratedCArtifact>,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let (status, color) = match &state {
        CGenerationState::NotStarted => ("Ready to generate after save".into(), Color::Muted),
        CGenerationState::Waiting => ("Waiting for 1.5 seconds of idle time…".into(), Color::Muted),
        CGenerationState::Running => ("Generating C outside the UI thread…".into(), Color::Info),
        CGenerationState::Ready => (
            "Generated artifact matches the saved Rust source".into(),
            Color::Success,
        ),
        CGenerationState::Blocked(message) => (message.clone(), Color::Warning),
        CGenerationState::Failed(message) => (message.clone(), Color::Error),
        CGenerationState::Stale(message) => (message.clone(), Color::Warning),
    };
    v_flex()
        .p_3()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new("Actual generated C (experimental)").size(LabelSize::Large))
        .child(Label::new(status).size(LabelSize::Small).color(color))
        .child(
            Label::new("This is low-level rustc_codegen_c output, not idiomatic C and not an ABI-equivalent teaching translation. Invalid Rust cannot be translated; use Conceptual for intent.")
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .child(
            h_flex()
                .gap_1()
                .child(
                    Button::new(
                        "generate-c-refresh",
                        if artifact.is_some() { "Refresh" } else { "Generate" },
                    )
                    .on_click(cx.listener(|panel, _, _window, cx| {
                        panel.schedule_generated_c(cx);
                    })),
                )
                .when(matches!(state, CGenerationState::Waiting | CGenerationState::Running), |this| {
                    this.child(
                        Button::new("generate-c-cancel", "Cancel").on_click(cx.listener(
                            |panel, _, _window, cx| panel.cancel_generated_c(cx),
                        )),
                    )
                })
                .when(artifact.is_some(), |this| {
                    this.child(
                        Button::new("open-generated-c", "Open full artifact").on_click(
                            cx.listener(|panel, _, window, cx| {
                                panel.open_generated_c(window, cx);
                            }),
                        ),
                    )
                }),
        )
        .when_some(artifact, |this, artifact| {
            this.child(
                Label::new(format!(
                    "Backend: {}\nSource hash: {}\nArtifact: {}",
                    artifact.backend,
                    artifact.source_hash,
                    artifact.path.display()
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            )
            .child(Label::new(artifact.code).size(LabelSize::XSmall).buffer_font(cx))
        })
        .into_any_element()
}

#[cfg(any())]
fn render_lessons(
    active_lesson: Option<String>,
    lesson_output: SharedString,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let lessons = [
        (
            "moves",
            "1 · Moves",
            "Move a String, observe the old name become invalid, then compare borrowing, cloning, and shared ownership.",
        ),
        (
            "nll",
            "2 · NLL",
            "See a shared borrow end at its final use instead of the closing brace.",
        ),
        (
            "partial_move",
            "3 · Partial moves",
            "Move one struct field and track which places remain available.",
        ),
        (
            "box_heap",
            "4 · Box and heap",
            "Separate the stack handle from its uniquely owned allocation.",
        ),
        (
            "rc",
            "5 · Rc",
            "Follow multiple handles to one reference-counted allocation.",
        ),
        (
            "refcell",
            "6 · RefCell",
            "Move a borrow rule from compile time to a runtime borrow flag.",
        ),
        (
            "rc_refcell",
            "7 · Rc<RefCell<_>>",
            "Combine shared ownership with runtime-checked mutation.",
        ),
        (
            "arc_mutex",
            "8 · Arc<Mutex<_>>",
            "Compare atomic sharing and exclusive synchronized mutation.",
        ),
    ];
    v_flex()
        .gap_2()
        .child(Label::new("Guided ownership lessons").size(LabelSize::Large))
        .child(
            Label::new("Each lesson is regenerated under your temporary directory. It cannot overwrite your project.")
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .when(active_lesson.is_some(), |this| {
            this.child(
                Button::new("run-active-lesson", "Run Cargo check for active lesson")
                    .on_click(cx.listener(|panel, _, window, cx| {
                        panel.run_lesson_check(window, cx);
                    })),
            )
        })
        .child(
            Label::new(lesson_output)
                .size(LabelSize::XSmall)
                .color(Color::Muted)
                .buffer_font(cx),
        )
        .children(lessons.into_iter().map(|(id, title, description)| {
            let is_active = active_lesson.as_deref() == Some(id);
            v_flex()
                .p_2()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(if is_active {
                    cx.theme().status().success
                } else {
                    cx.theme().colors().border_variant
                })
                .child(
                    Label::new(if is_active {
                        format!("● {title}")
                    } else {
                        title.to_owned()
                    })
                    .size(LabelSize::Small),
                )
                .child(Label::new(description).size(LabelSize::XSmall).color(Color::Muted))
                .child(
                    Button::new(
                        SharedString::from(format!("open-lesson-{id}")),
                        if is_active { "Reset and reopen" } else { "Open disposable lesson" },
                    )
                    .on_click(cx.listener(move |panel, _, window, cx| {
                        panel.open_lesson(id, window, cx);
                    })),
                )
        }))
        .into_any_element()
}

#[cfg(any())]
fn prepare_lesson_project(lesson_id: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
    let source = lesson_source(lesson_id)
        .ok_or_else(|| anyhow::anyhow!("unknown ownership lesson `{lesson_id}`"))?;
    let root = std::env::temp_dir()
        .join("rust-workbench-lessons")
        .join(lesson_id);
    let source_dir = root.join("src");
    std::fs::create_dir_all(&source_dir)?;
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname = \"rust_workbench_lesson_{lesson_id}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n"
        ),
    )?;
    let main_path = source_dir.join("main.rs");
    std::fs::write(&main_path, source)?;
    Ok((root, main_path))
}

#[cfg(any())]
async fn run_lesson_cargo_check(root: PathBuf) -> anyhow::Result<String> {
    let mut command = async_process::Command::new("cargo");
    command
        .args(["check", "--message-format=short"])
        .current_dir(root)
        .kill_on_drop(true);
    let output = command.output().await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let summary = if output.status.success() {
        "✓ Cargo check succeeded"
    } else {
        "Cargo check found a problem (this is expected in failing lessons)"
    };
    Ok(format!("{summary}\n{stdout}{stderr}"))
}

#[cfg(any())]
fn lesson_source(lesson_id: &str) -> Option<&'static str> {
    Some(match lesson_id {
        "moves" => {
            r#"fn main() {
    let values: Box<Vec<i32>> = Box::new(vec![1, 2, 3]);
    let shared = values;
    println!("new owner sees {} items", shared.len());
    println!("old owner sees {} items", values.len());
}
"#
        }
        "nll" => {
            r#"fn main() {
    let mut text = String::from("hello");
    let reader = &text;
    println!("reader: {reader}"); // the shared borrow ends after this last use
    text.push('!');
    println!("owner: {text}");
}
"#
        }
        "partial_move" => {
            r#"struct Pair { left: String, right: String }

fn consume(_: String) {}

fn main() {
    let pair = Pair { left: "left".into(), right: "right".into() };
    consume(pair.left);
    println!("right still works: {}", pair.right);
    println!("whole pair: {} / {}", pair.left, pair.right);
}
"#
        }
        "box_heap" => {
            r#"fn main() {
    let boxed = Box::new(vec![10, 20, 30]);
    let moved_box = boxed;
    println!("heap allocation through new owner: {moved_box:?}");
    println!("old Box handle: {boxed:?}");
}
"#
        }
        "rc" => {
            r#"use std::rc::Rc;

fn main() {
    let values = Rc::new(vec![1, 2, 3]);
    let first_owner = Rc::clone(&values);
    let second_owner = Rc::clone(&values);
    println!("{} owners, {:?} / {:?}", Rc::strong_count(&values), first_owner, second_owner);
}
"#
        }
        "refcell" => {
            r#"use std::cell::RefCell;

fn main() {
    let text = RefCell::new(String::from("hello"));
    text.borrow_mut().push('!');
    println!("{}", text.borrow());
    // Runtime rule: uncommenting both lines below makes the second borrow panic.
    // let first_writer = text.borrow_mut();
    // let second_writer = text.borrow_mut();
}
"#
        }
        "rc_refcell" => {
            r#"use std::{cell::RefCell, rc::Rc};

fn main() {
    let shared = Rc::new(RefCell::new(vec![1, 2]));
    let other_owner = Rc::clone(&shared);
    other_owner.borrow_mut().push(3);
    println!("both owners see {:?}", shared.borrow());
}
"#
        }
        "arc_mutex" => {
            r#"use std::{sync::{Arc, Mutex}, thread};

fn main() {
    let count = Arc::new(Mutex::new(0));
    let worker_count = Arc::clone(&count);
    let worker = thread::spawn(move || *worker_count.lock().unwrap() += 1);
    worker.join().unwrap();
    println!("count = {}", *count.lock().unwrap());
}
"#
        }
        _ => return None,
    })
}

fn state_symbol(state: &str) -> &'static str {
    match state {
        "available" => "●",
        "shared_borrowed" => "◇",
        "mutably_borrowed" => "◆",
        "moved" => "→",
        "partially_moved" => "◐",
        "dropped" => "×",
        _ => "·",
    }
}

fn loan_symbol(kind: &str) -> &'static str {
    if kind == "mutable" { "◆" } else { "◇" }
}

fn memory_symbol(storage: &str) -> &'static str {
    match storage {
        "stack" => "▣",
        "heap" => "⬡",
        "inline" => "▤",
        _ => "□",
    }
}

fn guided_event_explanation(kind: &str, place: &str) -> String {
    match kind {
        "move" => format!("Ownership of `{place}` transfers here; the old owner cannot be used."),
        "partial_move" => {
            format!("Only `{place}` transfers; unaffected fields may still be usable.")
        }
        "borrow_shared" => format!("A read-only view of `{place}` starts here."),
        "borrow_mutable" => format!("An exclusive mutable view of `{place}` starts here."),
        "borrow_activate" => format!("The reserved mutable view of `{place}` becomes active."),
        "borrow_end" => format!("The compiler no longer needs this view of `{place}` after here."),
        "reinitialize" => format!("A new value makes `{place}` available again."),
        "invalid_use" => format!("This use is rejected because `{place}` is no longer available."),
        "last_use" => format!("This is the final use of `{place}` on this control-flow path."),
        "drop" => format!("The value owned by `{place}` is destroyed here."),
        _ => format!("Ownership state changes for `{place}`."),
    }
}

fn beginner_memory_explanation(kind: &str) -> &'static str {
    match kind {
        "stack_binding" => "the local name and its directly stored handle",
        "box_allocation" => "one owner points to one heap value",
        "rc_allocation" => {
            "several handles may share this allocation; counters choose when it is freed"
        }
        "arc_allocation" => "thread-safe shared ownership using atomic counters",
        "ref_cell_state" => "a runtime flag enforces shared-versus-exclusive borrowing",
        "mutex_state" => "a lock permits one accessor at a time",
        "rw_lock_state" => "a lock permits many readers or one writer",
        "vec_header" => "pointer, current length, and allocated capacity",
        "vec_buffer" => "the elements live in a separate growable heap buffer",
        "string_header" => "pointer, UTF-8 byte length, and capacity",
        "string_buffer" => "the UTF-8 bytes live in a separate heap buffer",
        _ => "inline value; custom allocation semantics are not assumed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_problem(
        id: &str,
        binding: &str,
        binding_line: u32,
        error_line: u32,
    ) -> OwnershipProblem {
        OwnershipProblem {
            id: id.to_owned(),
            category: "mutable_while_shared".to_owned(),
            diagnostic_code: Some("E0502".to_owned()),
            message: format!("cannot borrow `{binding}` as mutable"),
            binding_name: binding.to_owned(),
            primary_range: lsp::Range::new(
                lsp::Position::new(error_line, 8),
                lsp::Position::new(error_line, 15),
            ),
            binding_range: lsp::Range::new(
                lsp::Position::new(binding_line, 12),
                lsp::Position::new(binding_line, 12 + binding.len() as u32),
            ),
            related_ranges: Vec::new(),
            related: Vec::new(),
            model_position: lsp::Position::new(error_line, 8),
            precision: "compiler_exact".to_owned(),
        }
    }

    #[test]
    fn ownership_source_hash_matches_rust_analyzer() {
        assert_eq!(ownership_source_hash(""), "cbf29ce484222325");
        assert_eq!(ownership_source_hash("hello"), "a430d84680aabd0b");
    }

    #[test]
    fn ownership_model_rejects_a_late_stale_response() {
        let source = "fn main() {}\n";
        let model = OwnershipModel {
            source_hash: ownership_source_hash(source),
            ..OwnershipModel::default()
        };
        assert!(ownership_model_matches_source(&model, source));
        assert!(!ownership_model_matches_source(
            &model,
            "fn main() { changed(); }\n"
        ));
    }

    #[test]
    fn schema_eleven_model_must_echo_the_locked_problem_id() {
        let model = OwnershipModel {
            schema_version: 11,
            selected_problem_id: Some("self-events-push".to_owned()),
            ..OwnershipModel::default()
        };

        assert!(ownership_model_matches_problem(
            &model,
            Some("self-events-push")
        ));
        assert!(!ownership_model_matches_problem(
            &model,
            Some("current-assignment")
        ));
        assert!(ownership_model_matches_problem(&model, None));
    }

    #[test]
    fn mutation_guide_keeps_field_target_and_selects_responsible_operation() {
        let operation = |id: &str, name: &str| rust_analyzer_ext::OwnershipOperationInsight {
            id: id.to_owned(),
            range: lsp::Range::new(lsp::Position::new(13, 8), lsp::Position::new(14, 20)),
            name: name.to_owned(),
            signature: format!("fn {name}(&mut self)"),
            receiver_type: Some("Vec<String>".to_owned()),
            required_access: "mutable_borrow".to_owned(),
            available_access: "shared access through &self".to_owned(),
            why_required: format!("{name} may mutate the collection"),
            documentation: None,
            effects: Vec::new(),
            effect_facts: Vec::new(),
            call_chain: vec![name.to_owned()],
            alternatives: Vec::new(),
            provenance: "resolved_signature".to_owned(),
            truncated: false,
        };
        let model = OwnershipModel {
            operations: vec![
                operation("unrelated", "clear"),
                operation("responsible", "push"),
            ],
            mutation_requirement: Some(rust_analyzer_ext::OwnershipMutationRequirement {
                target_place: "self.events".to_owned(),
                access_source: "&self".to_owned(),
                available_access: "shared_borrow".to_owned(),
                required_access: "mutable_borrow".to_owned(),
                operation_id: "responsible".to_owned(),
                operation_name: "push".to_owned(),
                explanation: "self.events is reached through &self".to_owned(),
                provenance: "compiler_diagnostic_and_resolved_signature".to_owned(),
            }),
            ..OwnershipModel::default()
        };

        let selected = selected_mutation_operation(&model).unwrap();
        assert_eq!(selected.name, "push");
        let requirement = model.mutation_requirement.as_ref().unwrap();
        assert_eq!(requirement.target_place, "self.events");
        assert_ne!(requirement.target_place, "self");
        assert_eq!(
            readable_available_access(&requirement.available_access),
            "shared access"
        );

        let mut coarse_problem = test_problem("self-events", "self", 12, 13);
        coarse_problem.category = "immutable_mutation".to_owned();
        let fallback_model = OwnershipModel {
            selected_place: Some("self.events".to_owned()),
            operations: vec![operation("responsible", "push")],
            ..OwnershipModel::default()
        };
        assert_eq!(
            resolved_problem_target(&coarse_problem, &fallback_model),
            "self.events"
        );
        assert_ne!(
            resolved_problem_target(&coarse_problem, &fallback_model),
            "self"
        );
    }

    #[test]
    fn ownership_repair_cannot_apply_before_compiler_validation() {
        let candidate = rust_analyzer_ext::OwnershipRepair {
            id: "rc-0".to_owned(),
            title: "Use Rc".to_owned(),
            strategy: "rc".to_owned(),
            semantics: "shared ownership".to_owned(),
            diff: "- Box<T>\n+ Rc<T>".to_owned(),
            compiler_validated: false,
            validation_state: "candidate".to_owned(),
            effects: rust_analyzer_ext::OwnershipRepairEffects::default(),
        };
        assert!(!repair_is_compiler_validated(&candidate));

        let validated = rust_analyzer_ext::OwnershipRepair {
            compiler_validated: true,
            validation_state: "candidate".to_owned(),
            ..candidate.clone()
        };
        assert!(repair_is_compiler_validated(&validated));

        let legacy: rust_analyzer_ext::OwnershipRepair =
            serde_json::from_value(serde_json::json!({
                "id": "rc-0",
                "title": "Use Rc",
                "strategy": "rc",
                "semantics": "shared ownership",
                "diff": "+ Rc<T>",
                "compilerValidated": true
            }))
            .unwrap();
        assert!(repair_is_compiler_validated(&legacy));
    }

    #[test]
    fn repair_result_tracks_the_selected_issue_when_other_errors_remain() {
        let verification = RepairVerification {
            diagnostic_code: Some("E0502".to_owned()),
            category: "mutable_while_shared".to_owned(),
            binding_name: "selected".to_owned(),
            original_line: 20,
            repair_title: "Narrow the borrow".to_owned(),
            baseline_problem_signatures: BTreeSet::from([
                "E0502|mutable_while_shared|selected".to_owned(),
                "E0502|mutable_while_shared|other".to_owned(),
            ]),
            state: RepairVerificationState::Checking,
        };
        let problems = OwnershipProblems {
            status: "ready".to_owned(),
            problems: vec![test_problem("other", "other", 40, 44)],
            ..OwnershipProblems::default()
        };
        assert!(matches!(
            repair_verification_outcome(&verification, &problems),
            RepairVerificationState::Resolved {
                remaining_file_problems: 1
            }
        ));
    }

    #[test]
    fn repair_result_does_not_claim_success_when_matching_error_remains_nearby() {
        let verification = RepairVerification {
            diagnostic_code: Some("E0502".to_owned()),
            category: "mutable_while_shared".to_owned(),
            binding_name: "selected".to_owned(),
            original_line: 20,
            repair_title: "Narrow the borrow".to_owned(),
            baseline_problem_signatures: BTreeSet::from([
                "E0502|mutable_while_shared|selected".to_owned()
            ]),
            state: RepairVerificationState::Checking,
        };
        let problems = OwnershipProblems {
            status: "ready".to_owned(),
            problems: vec![test_problem("same", "selected", 10, 22)],
            ..OwnershipProblems::default()
        };
        assert!(matches!(
            repair_verification_outcome(&verification, &problems),
            RepairVerificationState::StillPresent { current_line: 22 }
        ));
    }

    #[test]
    fn repair_result_rejects_new_diagnostics_after_the_selected_error_disappears() {
        let verification = RepairVerification {
            diagnostic_code: Some("E0596".to_owned()),
            category: "immutable_mutation".to_owned(),
            binding_name: "self.events".to_owned(),
            original_line: 13,
            repair_title: "Use a mutable receiver".to_owned(),
            baseline_problem_signatures: BTreeSet::from([
                "E0596|immutable_mutation|self.events".to_owned()
            ]),
            state: RepairVerificationState::Checking,
        };
        let mut introduced = test_problem("new-caller-error", "analytics", 50, 52);
        introduced.category = "immutable_mutation".to_owned();
        introduced.diagnostic_code = Some("E0596".to_owned());
        let problems = OwnershipProblems {
            status: "ready".to_owned(),
            problems: vec![introduced],
            ..OwnershipProblems::default()
        };

        assert!(matches!(
            repair_verification_outcome(&verification, &problems),
            RepairVerificationState::IntroducedProblems { summaries }
                if summaries.iter().any(|summary| summary.contains("analytics"))
        ));
    }

    #[test]
    fn ownership_problem_stories_are_specific_and_beginner_readable() {
        let (title, what, why) = problem_story("partial_move", "pair");
        assert!(title.contains("Part of `pair` moved"));
        assert!(what.contains("field"));
        assert!(why.contains("moved field"));

        let (title, _, why) = problem_story("multiple_mutable_borrows", "data");
        assert!(title.contains("two overlapping mutable borrows"));
        assert!(why.contains("one writer"));
    }

    #[test]
    fn display_profiles_have_quiet_progressive_and_complete_defaults() {
        let focus = RustOwnershipDisplayPreferences::focus();
        assert_eq!(focus.inline_diagnostics, RustInlineDiagnosticMode::Selected);
        assert!(!focus.show_type_hints);
        assert!(focus.show_moves && focus.show_borrows && focus.show_invalid_uses);
        assert!(!focus.show_last_uses && !focus.show_drops);

        let learn = RustOwnershipDisplayPreferences::learn();
        assert!(learn.show_type_hints && learn.show_lifetimes);
        assert!(learn.show_borrow_ends && learn.show_reinitializations);
        assert!(!learn.show_drops);

        let full = RustOwnershipDisplayPreferences::full();
        assert_eq!(full.inline_diagnostics, RustInlineDiagnosticMode::All);
        assert_eq!(full.scope, RustOwnershipHintScope::File);
        assert!(full.show_parameter_hints && full.show_adjustments && full.show_drops);
    }

    #[test]
    fn persisted_display_preferences_exclude_ephemeral_editor_state() {
        let mut preferences = RustOwnershipDisplayPreferences::learn();
        preferences.enabled = true;
        preferences.focus_rows = vec![(10, 20)];

        let serialized = serde_json::to_string(&preferences).unwrap();
        assert!(!serialized.contains("enabled"));
        assert!(!serialized.contains("focus_rows"));

        let restored: RustOwnershipDisplayPreferences = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored.profile, RustOwnershipDisplayProfile::Learn);
        assert!(restored.show_lifetimes);
        assert!(restored.show_ownership_coloring);
        assert!(!restored.enabled);
        assert!(restored.focus_rows.is_empty());
    }

    #[cfg(any())]
    #[test]
    fn core_lessons_explain_rust_rules_c_risks_and_runtime_costs() {
        for category in [
            "use_after_move",
            "partial_move",
            "immutable_mutation",
            "multiple_mutable_borrows",
            "mutable_while_shared",
        ] {
            let lesson = core_concept_lesson(category, "value");
            assert!(!lesson.rule.is_empty());
            assert!(lesson.c_risk.contains('C'));
            assert!(!lesson.example.is_empty());
            assert!(!lesson.runtime.is_empty());
        }
    }

    #[test]
    fn ownership_problem_selection_follows_the_nearest_cursor_range() {
        let problems = vec![
            test_problem("self-error", "self", 11, 13),
            test_problem("current-error", "current", 18, 21),
            test_problem("history-error", "self", 26, 27),
        ];

        assert_eq!(
            nearest_ownership_problem_index(&problems, lsp::Position::new(11, 14)),
            Some(0)
        );
        assert_eq!(
            nearest_ownership_problem_index(&problems, lsp::Position::new(18, 14)),
            Some(1)
        );
        assert_eq!(
            nearest_ownership_problem_index(&problems, lsp::Position::new(27, 10)),
            Some(2)
        );
    }

    #[test]
    fn locked_issue_changes_only_inside_an_exact_primary_diagnostic_range() {
        let problems = vec![
            test_problem("self-events-push", "self.events", 12, 13),
            test_problem("current-assignment", "*current", 19, 21),
        ];

        assert_eq!(
            ownership_problem_index_at_position(&problems, lsp::Position::new(13, 10)),
            Some(0)
        );
        assert_eq!(
            ownership_problem_index_at_position(&problems, lsp::Position::new(21, 10)),
            Some(1)
        );
        assert_eq!(
            ownership_problem_index_at_position(&problems, lsp::Position::new(12, 20)),
            None,
            "clicking the receiver signature must not replace the locked field diagnostic"
        );
        assert_eq!(
            ownership_problem_index_at_position(&problems, lsp::Position::new(17, 0)),
            None,
            "ordinary cursor movement must not choose the nearest issue"
        );
    }

    #[test]
    fn rescans_reconcile_duplicate_targets_by_diagnostic_and_source_proximity() {
        let mut previous = test_problem("old-clear", "self.events", 26, 27);
        previous.category = "immutable_mutation".to_owned();
        previous.diagnostic_code = Some("E0596".to_owned());

        let mut push = test_problem("new-push", "self.events", 12, 13);
        push.category = "immutable_mutation".to_owned();
        push.diagnostic_code = Some("E0596".to_owned());
        let mut clear = test_problem("new-clear", "self.events", 27, 28);
        clear.category = "immutable_mutation".to_owned();
        clear.diagnostic_code = Some("E0596".to_owned());

        assert_eq!(
            reconciled_ownership_problem_index(&previous, &[push, clear]),
            Some(1),
            "a rescan must not jump from clear_history back to track_order"
        );
    }

    #[test]
    fn late_model_responses_cannot_replace_the_locked_issue() {
        let request_a = ModelRequestKey {
            source_hash: "source".to_owned(),
            problem_id: Some("self-events".to_owned()),
            position: PointUtf16::new(13, 8),
            selection_epoch: 4,
        };
        let request_b = ModelRequestKey {
            problem_id: Some("current".to_owned()),
            position: PointUtf16::new(21, 8),
            selection_epoch: 5,
            ..request_a.clone()
        };

        assert!(!model_response_is_current(
            Some(&request_b),
            &request_a,
            Some("current"),
            5,
        ));
        assert!(!model_response_is_current(
            Some(&request_b),
            &request_b,
            Some("self-events"),
            5,
        ));
        assert!(model_response_is_current(
            Some(&request_b),
            &request_b,
            Some("current"),
            5,
        ));
    }

    #[test]
    fn guided_mutation_facts_name_the_field_and_use_self_only_as_the_access_route() {
        let mut problem = test_problem("self-events", "self.events", 12, 13);
        problem.category = "immutable_mutation".to_owned();
        problem.diagnostic_code = Some("E0596".to_owned());
        let model = OwnershipModel {
            selected_place: Some("self.events".to_owned()),
            mutation_requirement: Some(rust_analyzer_ext::OwnershipMutationRequirement {
                target_place: "self.events".to_owned(),
                access_source: "&self".to_owned(),
                available_access: "shared_borrow".to_owned(),
                required_access: "mutable_borrow".to_owned(),
                operation_id: "push-operation".to_owned(),
                operation_name: "push".to_owned(),
                explanation: "shared access cannot provide mutation".to_owned(),
                provenance: "compiler_diagnostic_and_resolved_signature".to_owned(),
            }),
            ..OwnershipModel::default()
        };

        let facts = guided_issue_facts(&problem, &model);
        assert_eq!(facts.target, "self.events");
        assert_eq!(facts.access_route.as_deref(), Some("&self"));
        assert_eq!(facts.operation.as_deref(), Some("push"));
        assert!(facts.headline.contains("self.events"));
        assert!(!facts.headline.contains("on `self`"));
        assert!(facts.state_summary.contains("alive"));
        assert!(facts.state_summary.contains("has not moved"));
    }

    #[test]
    fn preferred_mutability_repair_changes_the_receiver_not_the_field_type() {
        let mut problem = test_problem("self-events", "self.events", 12, 13);
        problem.category = "immutable_mutation".to_owned();
        problem.diagnostic_code = Some("E0596".to_owned());
        let model = OwnershipModel {
            mutation_requirement: Some(rust_analyzer_ext::OwnershipMutationRequirement {
                target_place: "self.events".to_owned(),
                access_source: "&self".to_owned(),
                available_access: "shared_borrow".to_owned(),
                required_access: "mutable_borrow".to_owned(),
                operation_id: "push-operation".to_owned(),
                operation_name: "push".to_owned(),
                explanation: "push needs exclusive mutable access".to_owned(),
                provenance: "compiler_diagnostic_and_resolved_signature".to_owned(),
            }),
            source_context: Some(rust_analyzer_ext::OwnershipSourceContext {
                file: "analytics.rs".to_owned(),
                breadcrumbs: vec![rust_analyzer_ext::OwnershipContextItem {
                    kind: "function".to_owned(),
                    label: "track_order".to_owned(),
                    range: Some(lsp::Range::default()),
                }],
                call_paths: Vec::new(),
                related_types: vec!["Analytics".to_owned()],
                provenance: "syntax".to_owned(),
                truncated: false,
            }),
            ..OwnershipModel::default()
        };

        let repair = preferred_mutability_repair(&problem, &model).unwrap();
        assert!(repair.diff.contains("fn track_order(&self"));
        assert!(repair.diff.contains("fn track_order(&mut self"));
        assert!(!repair.diff.contains("RefCell"));
        assert!(!repair.diff.contains("Rc<"));
        assert!(repair.impact.contains("no runtime borrow checks"));
    }

    #[test]
    fn ownership_problem_buttons_wrap_in_both_directions() {
        assert_eq!(relative_problem_index(None, 0, 1), None);
        assert_eq!(relative_problem_index(Some(0), 5, 1), Some(1));
        assert_eq!(relative_problem_index(Some(4), 5, 1), Some(0));
        assert_eq!(relative_problem_index(Some(0), 5, -1), Some(4));
    }

    #[test]
    fn panel_font_scale_uses_ten_percent_steps_and_safe_limits() {
        assert_eq!(
            adjusted_panel_font_scale(
                DEFAULT_PANEL_FONT_SCALE_PERCENT,
                PANEL_FONT_SCALE_STEP_PERCENT,
            ),
            110
        );
        assert_eq!(
            adjusted_panel_font_scale(110, -PANEL_FONT_SCALE_STEP_PERCENT),
            100
        );
        assert_eq!(
            adjusted_panel_font_scale(MIN_PANEL_FONT_SCALE_PERCENT, -10),
            80
        );
        assert_eq!(
            adjusted_panel_font_scale(MAX_PANEL_FONT_SCALE_PERCENT, 10),
            180
        );
    }

    #[test]
    fn generated_c_preview_truncates_on_a_utf8_boundary() {
        let preview =
            generated_c_preview("abc€def".to_owned(), std::path::Path::new("artifact.c"), 4);
        assert!(preview.starts_with("abc"));
        assert!(!preview.starts_with("abc€"));
        assert!(preview.contains("artifact.c"));
    }

    #[test]
    fn intent_matrix_distinguishes_clone_rc_refcell_and_threads() {
        assert!(
            intent_recommendation(IntentAnswers {
                independent_clone: Some(true),
                ..Default::default()
            })
            .starts_with("Clone")
        );
        assert!(
            intent_recommendation(IntentAnswers {
                multiple_owners: Some(true),
                mutation: Some(false),
                crosses_threads: Some(false),
                independent_clone: Some(false),
            })
            .contains("Rc<T>")
        );
        assert!(
            intent_recommendation(IntentAnswers {
                multiple_owners: Some(true),
                mutation: Some(true),
                crosses_threads: Some(false),
                independent_clone: Some(false),
            })
            .contains("Rc<RefCell<T>>")
        );
        assert!(
            intent_recommendation(IntentAnswers {
                multiple_owners: Some(true),
                mutation: Some(true),
                crosses_threads: Some(true),
                independent_clone: Some(false),
            })
            .contains("Arc<Mutex<T>>")
        );
    }

    #[test]
    fn bundled_concept_catalog_is_closed_and_checkpointed() {
        let lessons = learning_catalog::all_lessons();
        assert!(lessons.len() >= 20);
        let ids = lessons
            .iter()
            .map(|lesson| lesson.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), lessons.len(), "concept ids must be unique");
        for lesson in lessons {
            assert!(!lesson.title.is_empty());
            assert!(!lesson.rule.is_empty());
            assert!(!lesson.memory_model.is_empty());
            assert!(lesson.correct_choice < lesson.choices.len());
            for related in lesson.related {
                assert!(
                    ids.contains(related),
                    "{} links to missing {related}",
                    lesson.id
                );
            }
        }
    }

    #[test]
    fn every_supported_problem_family_has_a_precomputed_lesson_and_repairs() {
        for category in [
            "use_after_move",
            "partial_move",
            "multiple_mutable_borrows",
            "mutable_while_shared",
            "use_while_mutably_borrowed",
            "move_while_borrowed",
            "assign_while_borrowed",
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
        ] {
            let ids = learning_catalog::lesson_ids_for_problem(category, None);
            assert!(!ids.is_empty(), "missing lesson mapping for {category}");
            assert!(
                ids.iter().all(|id| learning_catalog::lesson(id).is_some()),
                "invalid lesson mapping for {category}"
            );
            assert!(
                !learning_catalog::repair_ideas(category).is_empty(),
                "missing repair ideas for {category}"
            );
        }
    }

    #[test]
    fn visual_fallback_is_a_three_step_compiler_story() {
        let problem = test_problem("type-error", "value", 4, 7);
        let moments = visual_moments(Some(&problem), &OwnershipModel::default());
        assert_eq!(moments.len(), 3);
        assert_eq!(moments[0].phase, "contract");
        assert_eq!(moments[1].phase, "operation_rejected");
        assert_eq!(moments[2].phase, "repair");
        assert_eq!(moments[1].range, problem.primary_range);
    }

    #[test]
    fn beginner_pointer_models_keep_owners_references_and_runtime_guards_distinct() {
        let cases = [
            ("Box<Vec<i32>>", "unique Box handle", "not the allocation"),
            ("&String", "non-owning pointer", "remains alive"),
            ("&mut String", "exclusive access", "remains alive"),
            (
                "Rc<String>",
                "strong count = symbolic N",
                "Rc::clone: N + 1",
            ),
            (
                "Arc<String>",
                "atomic strong count = symbolic N",
                "Arc::clone: N + 1",
            ),
            ("RefCell<String>", "runtime borrow flag", "can panic"),
            (
                "Rc<RefCell<String>>",
                "shared heap allocation",
                "runtime borrow flag",
            ),
            (
                "Arc<Mutex<String>>",
                "atomic strong count",
                "one lock holder",
            ),
            (
                "Arc<RwLock<String>>",
                "atomic strong count",
                "many readers or one writer",
            ),
        ];
        for (type_name, first_fact, second_fact) in cases {
            let model = smart_pointer_nodes("value", type_name).join("\n");
            assert!(
                model.contains(first_fact),
                "missing `{first_fact}` for {type_name}: {model}"
            );
            assert!(
                model.contains(second_fact),
                "missing `{second_fact}` for {type_name}: {model}"
            );
        }
    }

    #[test]
    fn beginner_trace_language_does_not_conflate_move_copy_clone_and_borrow() {
        assert_eq!(trace_arrow_label("move"), "ownership moves");
        assert_eq!(trace_arrow_label("copy"), "value copies");
        assert_eq!(trace_arrow_label("clone"), "clone returns");
        assert_eq!(trace_arrow_label("borrow_shared"), "shared reference");
        assert_eq!(trace_arrow_label("borrow_mutable"), "exclusive reference");
    }
}
