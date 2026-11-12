//! Preflight new wire routes against the saved sheet before any mutation.
use super::sch_connectivity::{net_graph_for, ConnectivityIndex};
use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use konnect_sexp::{
    geometry::point_on_segment,
    schematic::{extract_all_net_labels, extract_sheet_pins, extract_wires, Wire},
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
pub(crate) fn route_segments(a: Point, b: Point) -> Vec<Segment> {
    if (a.0 - b.0).abs() < TOL || (a.1 - b.1).abs() < TOL {
        vec![(a, b)]
    } else {
        vec![(a, (b.0, a.1)), ((b.0, a.1), b)]
    }
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
    if allow_join {
        return Ok(());
    }
    let wires = extract_wires(tree);
    let labels = extract_all_net_labels(tree);
    let mut graph = net_graph_for(tree, &wires, &labels);
    let index = ConnectivityIndex::build(tree, &wires, &[], &labels, TOL);
    let mut allowed = HashSet::new();
    for &(x, y) in endpoints {
        allowed.insert(graph.root_at(x, y));
    }
    for label in &labels {
        if named_net == Some(label.net.as_str()) {
            allowed.insert(graph.root_at(label.x, label.y));
        }
    }
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
    for label in &labels {
        let root = graph.root_at(label.x, label.y);
        if !allowed.contains(&root) && segments.iter().any(|s| on((label.x, label.y), *s)) {
            hits.push((label.net.clone(), (label.x, label.y)));
        }
    }
    for wire in &wires {
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
    if hits.is_empty() {
        return Ok(());
    }
    let mut nets: Vec<_> = hits.iter().map(|h| h.0.clone()).collect();
    nets.sort();
    nets.dedup();
    Err(CallToolResult::error_kind(
        ToolErrorKind::WouldJoinNets {
            nets,
            at: hits.into_iter().map(|(_, p)| [p.0, p.1]).collect(),
        },
        "Route would join unselected nets; inspect the route or explicitly set allow_join: true",
    ))
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
    let mut selected = endpoints.to_vec();
    for wire in extract_wires(tree) {
        if endpoints.iter().any(|point| on(*point, segment(&wire))) {
            selected.push((wire.x1, wire.y1));
        }
    }
    check_route(tree, segments, &selected, None, allow_join)
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
