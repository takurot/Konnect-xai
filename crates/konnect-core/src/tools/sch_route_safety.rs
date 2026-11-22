//! Preflight new wire routes against the saved sheet before any mutation.
use super::sch_connectivity::{
    net_graph_for, net_graph_for_physical_links, ConnectivityIndex, NetGraph, NetRoot,
};
use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use konnect_sexp::{
    geometry::point_on_segment,
    schematic::{
        extract_all_net_labels, extract_sheet_pins, extract_wires, Label, LabelKind, Wire,
    },
    SexpNode,
};
use std::collections::HashSet;
type Point = (f64, f64);
pub(crate) type Segment = (Point, Point);
const TOL: f64 = 0.01;
fn on(p: Point, s: Segment) -> bool {
    point_on_segment(p.0, p.1, s.0 .0, s.0 .1, s.1 .0, s.1 .1, TOL)
}
fn segment(w: &Wire) -> Segment {
    ((w.x1, w.y1), (w.x2, w.y2))
}
/// Horizontal-first, then vertical-first; a straight pair has one route.
fn route_candidates(a: Point, b: Point) -> Vec<Vec<Segment>> {
    if (a.0 - b.0).abs() < TOL || (a.1 - b.1).abs() < TOL {
        return vec![vec![(a, b)]];
    }
    [(b.0, a.1), (a.0, b.1)]
        .into_iter()
        .map(|corner| vec![(a, corner), (corner, b)])
        .collect()
}
fn intersection(a: Segment, b: Segment) -> Option<Point> {
    for point in [a.0, a.1] {
        if on(point, b) {
            return Some(point);
        }
    }
    for point in [b.0, b.1] {
        if on(point, a) {
            return Some(point);
        }
    }
    let r = (a.1 .0 - a.0 .0, a.1 .1 - a.0 .1);
    let s = (b.1 .0 - b.0 .0, b.1 .1 - b.0 .1);
    let cross = r.0 * s.1 - r.1 * s.0;
    if cross.abs() < 1e-12 {
        return None;
    }
    let d = (b.0 .0 - a.0 .0, b.0 .1 - a.0 .1);
    let t = (d.0 * s.1 - d.1 * s.0) / cross;
    let u = (d.0 * r.1 - d.1 * r.0) / cross;
    if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
        Some((a.0 .0 + t * r.0, a.0 .1 + t * r.1))
    } else {
        None
    }
}
pub(crate) fn allow_join(args: &serde_json::Value) -> Result<bool, CallToolResult> {
    match args.get("allow_join") {
        None => Ok(false),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| super::invalid_arg("allow_join", "must be a boolean")),
    }
}
/// Only the selected endpoint nets (and an explicitly named stub net) are
/// authorized. Geometric crossings are conservatively refused as well.
pub(crate) fn check_route(
    tree: &SexpNode,
    segments: &[Segment],
    endpoints: &[Point],
    named_net: Option<&str>,
    allow_join: bool,
) -> Result<(), CallToolResult> {
    choose_route(tree, &[segments.to_vec()], endpoints, named_net, allow_join).map(|_| ())
}

fn choose_route(
    tree: &SexpNode,
    candidates: &[Vec<Segment>],
    endpoints: &[Point],
    named_net: Option<&str>,
    allow_join: bool,
) -> Result<Vec<Segment>, CallToolResult> {
    if allow_join {
        return Ok(candidates[0].clone());
    }
    let wires = extract_wires(tree);
    let labels = extract_all_net_labels(tree);
    let graph = net_graph_for(tree, &wires, &labels);
    choose_route_on_graph(
        tree,
        candidates,
        endpoints,
        &wires,
        &labels,
        graph,
        |label| named_net == Some(label.net.as_str()),
    )
}

/// An L route between pin endpoints: horizontal-first unless only
/// vertical-first keeps clear of unselected nets.
pub(crate) fn select_route(
    tree: &SexpNode,
    a: Point,
    b: Point,
    allow_join: bool,
) -> Result<Vec<Segment>, CallToolResult> {
    choose_route(tree, &route_candidates(a, b), &[a, b], None, allow_join)
}

pub(crate) enum NamedStubRoute {
    AlreadyConnected,
    Write,
}

pub(crate) fn check_named_stub_route(
    tree: &SexpNode,
    segment: Segment,
    net: &str,
    label_kind: LabelKind,
    allow_join: bool,
) -> Result<NamedStubRoute, CallToolResult> {
    let wires = extract_wires(tree);
    let labels = extract_all_net_labels(tree);
    let mut graph = net_graph_for_physical_links(tree, &wires, &labels);
    let (x, y) = segment.0;
    let root = graph.root_at(x, y);
    let matches_request = |label: &Label| label.kind == label_kind && label.net == net;
    if labels
        .iter()
        .any(|label| matches_request(label) && graph.root_at(label.x, label.y) == root)
    {
        return Ok(NamedStubRoute::AlreadyConnected);
    }
    if !allow_join {
        let mut aliases = graph.aliases_of_root(root);
        if !aliases.is_empty() {
            aliases.insert(net.to_owned());
            return Err(CallToolResult::error_kind(
                ToolErrorKind::WouldJoinNets {
                    nets: aliases.into_iter().collect(),
                    at: vec![[x, y]],
                },
                "The selected endpoint uses a different net name or label scope. Set allow_join: true to join the nets explicitly.",
            ));
        }
        choose_route_on_graph(
            tree,
            &[vec![segment]],
            &[(x, y)],
            &wires,
            &labels,
            graph,
            matches_request,
        )?;
    }
    Ok(NamedStubRoute::Write)
}

/// The first candidate that joins no unselected net. When every candidate
/// would, one refusal carries the hits of all of them.
fn choose_route_on_graph(
    tree: &SexpNode,
    candidates: &[Vec<Segment>],
    endpoints: &[Point],
    wires: &[Wire],
    labels: &[Label],
    mut graph: NetGraph,
    matches_request: impl Fn(&Label) -> bool,
) -> Result<Vec<Segment>, CallToolResult> {
    let index = ConnectivityIndex::build(tree, wires, &[], labels, TOL);
    let mut allowed = HashSet::new();
    for &(x, y) in endpoints {
        allowed.insert(graph.root_at(x, y));
    }
    for label in labels {
        if matches_request(label) {
            allowed.insert(graph.root_at(label.x, label.y));
        }
    }
    let mut hits = Vec::new();
    for segments in candidates {
        let route_hits = foreign_hits(tree, segments, &index, &mut graph, &allowed, wires, labels);
        if route_hits.is_empty() {
            return Ok(segments.clone());
        }
        let earlier = hits.len();
        for hit in route_hits {
            if !hits[..earlier].contains(&hit) {
                hits.push(hit);
            }
        }
    }
    let message = if candidates.len() > 1 {
        "Both the horizontal-first and the vertical-first route would join unselected nets; inspect the routes or explicitly set allow_join: true"
    } else {
        "Route would join unselected nets; inspect the route or explicitly set allow_join: true"
    };
    let mut nets: Vec<_> = hits.iter().map(|h| h.0.clone()).collect();
    nets.sort();
    nets.dedup();
    Err(CallToolResult::error_kind(
        ToolErrorKind::WouldJoinNets {
            nets,
            at: hits.into_iter().map(|(_, p)| [p.0, p.1]).collect(),
        },
        message,
    ))
}

fn foreign_hits(
    tree: &SexpNode,
    segments: &[Segment],
    index: &ConnectivityIndex,
    graph: &mut NetGraph,
    allowed: &HashSet<NetRoot>,
    wires: &[Wire],
    labels: &[Label],
) -> Vec<(String, Point)> {
    let mut hits = Vec::new();
    for pin in index.placed_pins() {
        let root = graph.root_at(pin.at.0, pin.at.1);
        if !allowed.contains(&root) && segments.iter().any(|s| on(pin.at, *s)) {
            let name = graph
                .name_of_root(root)
                .unwrap_or_else(|| format!("{}.{}", pin.reference, pin.pin.number));
            hits.push((name, pin.at));
        }
    }
    for point in extract_sheet_pins(tree) {
        let root = graph.root_at(point.0, point.1);
        if !allowed.contains(&root) && segments.iter().any(|s| on(point, *s)) {
            hits.push((
                graph
                    .name_of_root(root)
                    .unwrap_or_else(|| "sheet_pin".into()),
                point,
            ));
        }
    }
    for label in labels {
        let root = graph.root_at(label.x, label.y);
        if !allowed.contains(&root) && segments.iter().any(|s| on((label.x, label.y), *s)) {
            hits.push((label.net.clone(), (label.x, label.y)));
        }
    }
    for wire in wires {
        let root = graph.root_at(wire.x1, wire.y1);
        if allowed.contains(&root) {
            continue;
        }
        if let Some(at) = segments
            .iter()
            .find_map(|s| intersection(*s, segment(wire)))
        {
            hits.push((
                graph.name_of_root(root).unwrap_or_else(|| {
                    format!("wire:{}", wire.uuid.as_deref().unwrap_or("unnamed"))
                }),
                at,
            ));
        }
    }
    hits
}
/// Coordinate-selected endpoints explicitly select wires passing that point.
/// Pin-selected endpoints must use `check_route`: an undotted pin midspan
/// does not already belong to the wire's net.
pub(crate) fn check_coordinate_route(
    tree: &SexpNode,
    segments: &[Segment],
    endpoints: &[Point],
    allow_join: bool,
) -> Result<(), CallToolResult> {
    choose_route(
        tree,
        &[segments.to_vec()],
        &coordinate_selection(tree, endpoints),
        None,
        allow_join,
    )
    .map(|_| ())
}

/// `select_route` for coordinate-selected endpoints.
pub(crate) fn select_coordinate_route(
    tree: &SexpNode,
    a: Point,
    b: Point,
    allow_join: bool,
) -> Result<Vec<Segment>, CallToolResult> {
    choose_route(
        tree,
        &route_candidates(a, b),
        &coordinate_selection(tree, &[a, b]),
        None,
        allow_join,
    )
}

fn coordinate_selection(tree: &SexpNode, endpoints: &[Point]) -> Vec<Point> {
    let mut selected = endpoints.to_vec();
    for wire in extract_wires(tree) {
        if endpoints.iter().any(|point| on(*point, segment(&wire))) {
            selected.push((wire.x1, wire.y1));
        }
    }
    selected
}

/// Junction candidates made by this wire only. Never repair unrelated tees.
pub(crate) fn local_junctions(wires: &[Wire], new: &Wire) -> Vec<Point> {
    let mut positions = Vec::new();
    for old in wires {
        for p in [(old.x1, old.y1), (old.x2, old.y2)] {
            if on(p, segment(new)) && p != (new.x1, new.y1) && p != (new.x2, new.y2) {
                positions.push(p);
            }
        }
        for p in [(new.x1, new.y1), (new.x2, new.y2)] {
            if on(p, segment(old)) && p != (old.x1, old.y1) && p != (old.x2, old.y2) {
                positions.push(p);
            }
        }
    }
    positions
}

#[cfg(test)]
#[path = "sch_route_safety_tests.rs"]
mod tests;
