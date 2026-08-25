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

pub(super) fn derive_beginner_memory_path(
    scene: &OwnershipTopologyScene,
) -> BeginnerMemoryPath {
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
        let first = &scene.nodes[0];
        distance.insert(first.id.clone(), 0);
        queue.push_back(first.id.clone());
    }

    while let Some(source) = queue.pop_front() {
        let source_distance = distance[&source];
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
                distance.insert(edge.target.clone(), source_distance + 1);
                queue.push_back(edge.target.clone());
            }
        }
    }

    let mut next_distance = distance.values().copied().max().unwrap_or(0) + 1;
    for node in &scene.nodes {
        if !distance.contains_key(&node.id) {
            distance.insert(node.id.clone(), next_distance);
            next_distance += 1;
        }
    }

    let mut grouped = BTreeMap::<usize, Vec<TopologyNode>>::new();
    for node in &scene.nodes {
        grouped
            .entry(distance[&node.id])
            .or_default()
            .push(node.clone());
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
    for snapshot in model.memory_graph.snapshots.iter().take(selected_step + 1) {
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
    let snapshots = model.memory_graph.snapshots.iter().take(selected_step + 1);
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
        adjacent.sort_by(|left, right| left.cmp(right));
        for (_, _, adjacent_id) in adjacent {
            if !visited.contains(&adjacent_id) {
                queue.push_back(adjacent_id);
            }
        }
    }
    selected
}

fn layout_topology_scene(nodes: &mut [TopologyNode], edges: &mut [TopologyEdge]) -> u16 {
    for (row, node) in nodes.iter_mut().enumerate() {
        let (x, width) = match node.column {
            TopologyColumn::Local => (8, 404),
            TopologyColumn::Wrapper => (36, 376),
            TopologyColumn::Target => (64, 348),
        };
        node.rect = TopologyRect {
            x,
            y: TOPOLOGY_ROW_START + u16::try_from(row).unwrap_or(u16::MAX) * TOPOLOGY_ROW_STRIDE,
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
        let source_center_x = source.x + source.width / 2;
        let target_center_x = target.x + target.width / 2;
        if source.y < target.y {
            let start = (source_center_x, source.y + source.height);
            let end = (target_center_x, target.y);
            let middle_y = start.1 + end.1.saturating_sub(start.1) / 2;
            edge.route = vec![start, (start.0, middle_y), (end.0, middle_y), end];
            edge.label_position = (start.0.min(end.0) + 8, middle_y.saturating_sub(8));
        } else {
            let side_x = TOPOLOGY_CANVAS_WIDTH.saturating_sub(2);
            let start = (source.x + source.width, source.y + source.height / 2);
            let end = (target.x + target.width, target.y + target.height / 2);
            edge.route = vec![start, (side_x, start.1), (side_x, end.1), end];
            edge.label_position = (side_x.saturating_sub(104), start.1.min(end.1) + 8);
        }
    }

    TOPOLOGY_ROW_START + u16::try_from(nodes.len()).unwrap_or(u16::MAX).max(1) * TOPOLOGY_ROW_STRIDE
}

pub(super) fn derive_ownership_topology_scene(
    problem: Option<&OwnershipProblem>,
    model: &OwnershipModel,
    selected_step: usize,
) -> Option<OwnershipTopologyScene> {
    derive_ownership_topology_scene_with_limits(problem, model, selected_step, 12, 20, false)
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
                node(
                    "vec-header",
                    "wrapper",
                    "inline",
                    "Vec header",
                    "Vec<i32>",
                ),
                node(
                    "vec-buffer",
                    "buffer",
                    "heap",
                    "Vec element buffer",
                    "i32",
                ),
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
}
