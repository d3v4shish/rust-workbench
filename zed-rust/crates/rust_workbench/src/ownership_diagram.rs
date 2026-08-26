use super::ownership_topology::{
    OwnershipTopologyScene, TOPOLOGY_CANVAS_WIDTH, TopologyEdge, TopologyNode,
    topology_column_title,
};
use super::*;

fn topology_node_color(state: &str) -> Color {
    if state.contains("blocked") || state.contains("reject") || state.contains("invalid") {
        Color::Error
    } else if state.contains("possible")
        || state.contains("hypothetical")
        || state.contains("requested")
    {
        Color::Warning
    } else if state.contains("borrow") || state.contains("read-only") {
        Color::Info
    } else if state.contains("move") || state.contains("drop") {
        Color::Warning
    } else {
        Color::Success
    }
}

fn node_role(node: &TopologyNode) -> &'static str {
    if node.kind == "binding" {
        "VARIABLE"
    } else if node.kind == "handle" {
        "HANDLE"
    } else if node.kind == "wrapper" {
        "WRAPPER"
    } else if node.kind == "operation" {
        "OPERATION"
    } else if node.kind == "reference_binding" {
        "REFERENCE"
    } else if node.kind == "future_state" {
        "FUTURE STATE"
    } else if node.kind == "suspension_point" {
        "AWAIT POINT"
    } else if node.kind == "closure_environment" {
        "CLOSURE"
    } else if node.kind == "metadata" {
        "METADATA"
    } else if node.kind == "element" {
        "ELEMENT"
    } else if node.storage == "heap" {
        "HEAP"
    } else {
        topology_column_title(node.column)
    }
}

fn provenance_symbol(provenance: &str) -> &'static str {
    if provenance.contains("conceptual") {
        "?"
    } else if matches!(
        provenance,
        "compiler_exact" | "compiler_diagnostic" | "resolved_self_parameter"
    ) {
        "✓"
    } else {
        "≈"
    }
}

fn render_topology_node(
    node: TopologyNode,
    selected_element: Option<&str>,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let color = topology_node_color(&node.state);
    let rect = node.rect;
    let selected = selected_element == Some(node.id.as_str());
    let node_id = node.id.clone();
    let range = node.range;
    let accessible_name = format!(
        "{} {}; type {}; state {}; {}; provenance {}",
        node_role(&node),
        node.label,
        node.type_name,
        node.state,
        node.detail,
        node.provenance
    );
    let inspection_summary = format!(
        "Inspecting `{}`: {}. This does not change the selected compiler issue.",
        node.label, node.state
    );
    v_flex()
        .absolute()
        .left(px(f32::from(rect.x)))
        .top(px(f32::from(rect.y)))
        .w(px(f32::from(rect.width)))
        .h(px(f32::from(rect.height)))
        .overflow_hidden()
        .px_2()
        .py_1()
        .gap_0p5()
        .rounded_md()
        .border_1()
        .border_color(match color {
            Color::Error => cx.theme().status().error,
            Color::Warning => cx.theme().status().warning,
            Color::Info => cx.theme().status().info,
            _ => cx.theme().status().success,
        })
        .bg(if node.provenance.contains("conceptual") {
            cx.theme().status().warning_background.opacity(0.12)
        } else {
            match node.storage.as_str() {
                "heap" => cx.theme().status().success_background.opacity(0.08),
                "inline" => cx.theme().status().info_background.opacity(0.06),
                _ => cx.theme().colors().panel_background,
            }
        })
        .child(
            Button::new(
                SharedString::from(format!("topology-node-{}", node.id)),
                format!(
                    "{} {} · {} `{}`",
                    provenance_symbol(&node.provenance),
                    node_role(&node),
                    visual_state_symbol(&node.state),
                    node.label
                ),
            )
            .toggle_state(selected)
            .aria_label(accessible_name.clone())
            .aria_description("Select this value layer and reveal its source range")
            .tooltip(ui::Tooltip::text(accessible_name))
            .on_click(cx.listener(move |panel, _, window, cx| {
                panel.inspect_topology_element_and_focus(
                    node_id.clone(),
                    inspection_summary.clone(),
                    range,
                    window,
                    cx,
                )
            })),
        )
        .child(
            Label::new(node.type_name)
                .size(LabelSize::XSmall)
                .buffer_font(cx),
        )
        .child(
            Label::new(format!("{} · {}", node.detail, node.state))
                .size(LabelSize::XSmall)
                .color(color),
        )
        .into_any_element()
}

fn render_topology_edge_canvas(edges: Vec<TopologyEdge>) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, cx| {
            for edge in edges {
                if edge.route.len() < 2 {
                    continue;
                }
                let mut builder = if !edge.active
                    || edge.provenance.contains("conceptual")
                    || edge.label.contains("weak")
                    || edge.label.contains("conditional")
                {
                    PathBuilder::stroke(px(1.)).dash_array(&[px(4.), px(3.)])
                } else {
                    PathBuilder::stroke(px(1.5))
                };
                for (index, (x, y)) in edge.route.iter().copied().enumerate() {
                    let point = point(
                        bounds.origin.x + px(f32::from(x)),
                        bounds.origin.y + px(f32::from(y)),
                    );
                    if index == 0 {
                        builder.move_to(point);
                    } else {
                        builder.line_to(point);
                    }
                }
                let mut route_from_end = edge.route.iter().rev().copied();
                let (Some((end_x, end_y)), Some((before_x, before_y))) =
                    (route_from_end.next(), route_from_end.next())
                else {
                    continue;
                };
                let end = point(
                    bounds.origin.x + px(f32::from(end_x)),
                    bounds.origin.y + px(f32::from(end_y)),
                );
                let arrow = 5.0;
                if before_x < end_x {
                    builder.move_to(point(end.x - px(arrow), end.y - px(arrow)));
                    builder.line_to(end);
                    builder.line_to(point(end.x - px(arrow), end.y + px(arrow)));
                } else if before_x > end_x {
                    builder.move_to(point(end.x + px(arrow), end.y - px(arrow)));
                    builder.line_to(end);
                    builder.line_to(point(end.x + px(arrow), end.y + px(arrow)));
                } else if before_y <= end_y {
                    builder.move_to(point(end.x - px(arrow), end.y - px(arrow)));
                    builder.line_to(end);
                    builder.line_to(point(end.x + px(arrow), end.y - px(arrow)));
                } else {
                    builder.move_to(point(end.x - px(arrow), end.y + px(arrow)));
                    builder.line_to(end);
                    builder.line_to(point(end.x + px(arrow), end.y + px(arrow)));
                }
                if let Ok(path) = builder.build() {
                    let color =
                        if edge.label.contains("mutable") || edge.label.contains("exclusive") {
                            cx.theme().status().warning
                        } else if edge.label.contains("borrow") {
                            cx.theme().status().info
                        } else if edge.label.contains("realloc")
                            || edge.label.contains("invalidate")
                            || edge.label.contains("suspend")
                        {
                            cx.theme().status().warning
                        } else if edge.label.contains("owns")
                            || edge.label.contains("shares")
                            || edge.label == "stores"
                        {
                            cx.theme().status().success
                        } else {
                            cx.theme().colors().text_muted
                        };
                    window.paint_path(
                        path,
                        if edge.active {
                            color
                        } else {
                            color.opacity(0.35)
                        },
                    );
                }
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

fn diagram_phase_label(moment: &super::ownership_topology::TopologyMoment) -> String {
    match moment.path_marker.as_deref() {
        Some("borrow_created" | "borrow_reserved" | "before") => "Before".to_owned(),
        Some("operation_rejected" | "conflict") => "Conflict".to_owned(),
        Some("borrow_ended" | "after" | "result") => "After".to_owned(),
        Some("operation") => "Operation".to_owned(),
        Some("shape") => "Current shape".to_owned(),
        _ if moment.title.to_ascii_lowercase().contains("suspend") => "Suspended".to_owned(),
        _ if moment.title.to_ascii_lowercase().contains("resume") => "Resumed".to_owned(),
        _ => moment.title.clone(),
    }
}

fn render_edge_label(edge: TopologyEdge, cx: &App) -> AnyElement {
    gpui::div()
        .absolute()
        .left(px(f32::from(edge.label_position.0)))
        .top(px(f32::from(edge.label_position.1)))
        .px_1()
        .rounded_sm()
        .bg(cx.theme().colors().panel_background)
        .child(
            Label::new(edge.label.replace('_', " "))
                .size(LabelSize::XSmall)
                .color(if edge.active {
                    Color::Muted
                } else {
                    Color::Disabled
                }),
        )
        .into_any_element()
}

pub(super) fn render_topology_scene(
    scene: OwnershipTopologyScene,
    selected_element: Option<&str>,
    cx: &mut Context<RustWorkbenchPanel>,
) -> AnyElement {
    let selected_moment = scene.moments.get(scene.selected_step).cloned();
    let canvas_edges = scene.edges.clone();
    let linear_flow = scene
        .nodes
        .iter()
        .map(|node| format!("{} {}", node_role(node).to_lowercase(), node.label))
        .collect::<Vec<_>>()
        .join(" → ");
    v_flex()
        .p_2()
        .gap_1()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(
            v_flex()
                .gap_0p5()
                .child(
                    Label::new(scene.title.clone())
                    .size(LabelSize::Small),
                )
                .child(
                    Label::new("✓ compiler  ≈ derived  ? possible")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
        )
        .child(
            Label::new(scene.summary.clone())
                .size(LabelSize::Small),
        )
        .when(!scene.moments.is_empty(), |this| {
            this.child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .children(scene.moments.iter().enumerate().map(|(index, moment)| {
                        let label = diagram_phase_label(moment);
                        Button::new(
                            SharedString::from(format!("topology-step-{index}")),
                            if index == scene.selected_step {
                                format!("● {label}")
                            } else {
                                label
                            },
                        )
                        .toggle_state(index == scene.selected_step)
                        .aria_label(format!(
                            "Show diagram phase {} of {}: {}",
                            index + 1,
                            scene.moments.len(),
                            moment.title
                        ))
                        .tooltip(ui::Tooltip::text(moment.explanation.clone()))
                        .on_click(cx.listener(move |panel, _, _window, cx| {
                            panel.select_visual_step(index, cx)
                        }))
                    })),
            )
        })
        .when_some(selected_moment, |this, moment| {
            this.child(
                v_flex()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .border_l_2()
                    .border_color(if moment
                        .path_marker
                        .as_deref()
                        .is_some_and(|phase| phase.contains("reject") || phase == "conflict")
                    {
                        cx.theme().status().error
                    } else {
                        cx.theme().status().info
                    })
                    .child(
                        Label::new(format!(
                            "{} · line {}",
                            diagram_phase_label(&moment),
                            display_line_number(moment.range.start.line)
                        ))
                        .size(LabelSize::XSmall),
                    )
                    .child(
                        Label::new(moment.explanation)
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
        })
        .child(
            Label::new("LOCAL / REFERENCE  ↓  INLINE OWNER OR WRAPPER  ↓  HEAP / RUNTIME TARGET")
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .when(scene.legacy_limited, |this| {
            this.child(
                Label::new("This cached compiler model predates explicit wrapper handles. Refresh to rebuild the diagram with schema 7 facts.")
                    .size(LabelSize::XSmall)
                    .color(Color::Warning),
            )
        })
        .child(
            gpui::div()
                .relative()
                .w(px(f32::from(TOPOLOGY_CANVAS_WIDTH)))
                .max_w_full()
                .h(px(f32::from(scene.canvas_height)))
                .child(render_topology_edge_canvas(canvas_edges))
                .children(scene.edges.iter().cloned().map(|edge| render_edge_label(edge, cx)))
                .children(
                    scene
                        .nodes
                        .iter()
                        .cloned()
                        .map(|node| render_topology_node(node, selected_element, cx)),
                ),
        )
        .when(scene.expanded && !linear_flow.is_empty(), |this| {
            this.child(
                Label::new(format!("Value flow: {linear_flow}"))
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
        })
        .when(scene.expanded && !scene.edges.is_empty(), |this| {
            this.child(Label::new("Relations").size(LabelSize::Small)).child(
                h_flex().gap_1().flex_wrap().children(scene.edges.iter().map(|edge| {
                    let source = scene
                        .nodes
                        .iter()
                        .find(|node| node.id == edge.source)
                        .map(|node| node.label.as_str())
                        .unwrap_or("?");
                    let target = scene
                        .nodes
                        .iter()
                        .find(|node| node.id == edge.target)
                        .map(|node| node.label.as_str())
                        .unwrap_or("?");
                    let edge_id = edge.id.clone();
                    let range = edge.range;
                    let summary = format!(
                        "`{source}` {} `{target}`. Provenance: {}.",
                        edge.label.replace('_', " "),
                        edge.provenance.replace('_', " ")
                    );
                    Button::new(
                        SharedString::from(format!("topology-edge-{}", edge.id)),
                        format!(
                            "{} {source} · {} · {target}",
                            if edge.active { "●" } else { "○" },
                            edge.label.replace('_', " "),
                        ),
                    )
                    .toggle_state(selected_element == Some(edge.id.as_str()))
                    .aria_label(summary.clone())
                    .aria_description("Select this ownership relation and reveal its source")
                    .tooltip(ui::Tooltip::text(summary.clone()))
                    .on_click(cx.listener(move |panel, _, window, cx| {
                        panel.inspect_topology_element_and_focus(
                            edge_id.clone(),
                            summary.clone(),
                            range,
                            window,
                            cx,
                        )
                    }))
                })),
            )
        })
        .when(!scene.access_lines.is_empty(), |this| {
            this.child(Label::new("How Rust reaches the value").size(LabelSize::Small))
                .children(scene.access_lines.into_iter().map(|line| {
                    Label::new(line).size(LabelSize::XSmall).color(Color::Muted)
                }))
        })
        .when(scene.truncated, |this| {
            this.child(
                Label::new(if scene.expanded {
                    "The full view is bounded to 64 nodes and 96 relations."
                } else {
                    "The compact view keeps the selected semantic path and is bounded to 12 nodes and 20 relations."
                })
                .size(LabelSize::XSmall)
                .color(Color::Warning),
            )
        })
        .into_any_element()
}
