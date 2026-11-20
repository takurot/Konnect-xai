//! `sch_analysis` toolset — net connectivity, pin queries, trace paths, overlap/orphan detection.
//!
//! All operations are read-only S-expression analysis. Connectivity — the net
//! graph and what counts as attached at a point — lives in `sch_connectivity`.

use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::tool;
use crate::tools::sch_connectivity::{
    analysis_net_graph, analysis_net_labels, analysis_net_name, label_roots, schematic_point,
    ConnectivityIndex,
};
use crate::tools::{
    get_path, is_power_symbol_reference, opt_f64, opt_positive_f64, placed_pins_by_reference,
    require_f64, require_str, ToolContext, ToolDef,
};
use konnect_schematic_editor as cse;
use konnect_sexp::{
    geometry::{point_on_segment, points_coincident, round6},
    schematic::{
        extract_buses, extract_sheet_pins, extract_symbol_instances, extract_wires, read_schematic,
        Label, LabelKind, LibPin, Wire,
    },
};
use serde_json::json;
use std::collections::{BTreeSet, HashMap, HashSet};

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "list_schematic_wires",
            "List all wire segments in a schematic with start/end coordinates and UUIDs.",
            json!({ "type": "object",
                "properties": { "schematic": { "type": "string" } },
                "required": ["schematic"] }),
            |args, ctx| async move { handle_list_wires(args, ctx).await }
        ),
        tool!(
            "list_schematic_nets",
            "List the saved sheet's named aliases and generated names for unlabeled connections.",
            json!({ "type": "object",
                "properties": { "schematic": { "type": "string" } },
                "required": ["schematic"] }),
            |args, ctx| async move { handle_list_nets(args, ctx).await }
        ),
        tool!(
            "list_schematic_labels",
            "List all label instances (net_label, global_label, hierarchical_label) \
             with their positions, net names, and types.",
            json!({ "type": "object",
                "properties": { "schematic": { "type": "string" } },
                "required": ["schematic"] }),
            |args, ctx| async move { handle_list_labels(args, ctx).await }
        ),
        tool!(
            "get_net_connections",
            "Get the component pins and labels for an exact net name on the requested schematic sheet.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "net": { "type": "string", "description": "Net name to query" }
                },
                "required": ["schematic", "net"] }),
            |args, ctx| async move { handle_get_net_connections(args, ctx).await }
        ),
        tool!(
            "get_net_connectivity",
            "Get the wires, labels, and T-junction locations for an exact net name on the requested schematic sheet.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "net": { "type": "string" }
                },
                "required": ["schematic", "net"] }),
            |args, ctx| async move { handle_get_net_connectivity(args, ctx).await }
        ),
        tool!(
            "get_pin_connections",
            "Get the net connected to a specific pin on a component by tracing wires from the pin endpoint.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "reference": { "type": "string" },
                    "pin_number": { "type": "string" }
                },
                "required": ["schematic", "reference", "pin_number"] }),
            |args, ctx| async move { handle_get_pin_connections(args, ctx).await }
        ),
        tool!(
            "get_pin_net_name",
            "Return just the net name for a specific pin on a component.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "reference": { "type": "string" },
                    "pin_number": { "type": "string" }
                },
                "required": ["schematic", "reference", "pin_number"] }),
            |args, ctx| async move { handle_get_pin_connections(args, ctx).await }
        ),
        tool!(
            "get_component_nets",
            "Get all nets connected to every pin of a component.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "reference": { "type": "string" }
                },
                "required": ["schematic", "reference"] }),
            |args, ctx| async move { handle_get_component_nets(args, ctx).await }
        ),
        tool!(
            "get_net_components",
            "Get all components (and their pins) connected to a named net.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "net": { "type": "string" }
                },
                "required": ["schematic", "net"] }),
            |args, ctx| async move { handle_get_net_components(args, ctx).await }
        ),
        tool!(
            "trace_from_point",
            "Trace connectivity from any (X,Y) point — returns the wires, labels, component \
             pins and junction dots at that point, and the net it belongs to. Hierarchical \
             sheet pins and no-connect flags are not reported, so an empty pins_here does \
             not prove a wire dangles.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "x": { "type": "number" }, "y": { "type": "number" },
                    "tolerance": {
                        "type": "number", "exclusiveMinimum": 0, "default": 0.05
                    }
                },
                "required": ["schematic", "x", "y"] }),
            |args, ctx| async move { handle_trace_from_point(args, ctx).await }
        ),
        tool!(
            "find_orphan_items",
            "Find dangling wire ends, floating labels, and unconnected pin endpoints. \
             Exact saved points determine electrical connection. Nearby orphan candidates are reported in near_misses.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "tolerance": {
                        "type": "number", "exclusiveMinimum": 0, "default": 0.05,
                        "description": "Distance in millimetres used only to detect near misses"
                    }
                },
                "required": ["schematic"] }),
            |args, ctx| async move { handle_find_orphan_items(args, ctx).await }
        ),
        tool!(
            "find_shorted_nets",
            "Detect accidentally merged nets — distinct net names that KiCad nets together, \
             through a wire path they share or through a name that joins their segments.",
            json!({ "type": "object",
                "properties": { "schematic": { "type": "string" } },
                "required": ["schematic"] }),
            |args, ctx| async move { handle_find_shorted_nets(args, ctx).await }
        ),
        tool!(
            "find_single_pin_nets",
            "Find nets that reach at most one pin — often a missing counterpart, an orphan \
             label, or a stub left by a deleted component. Component pins and hierarchical \
             sheet pins count; a power symbol's own pin names the rail rather than consuming \
             it and does not. Reports the pin and label counts, and every label kind that \
             named the net. Read per sheet: a net a global, hierarchical or power label can \
             carry off this one is flagged cross_sheet_unverified.",
            json!({ "type": "object",
                "properties": { "schematic": { "type": "string" } },
                "required": ["schematic"] }),
            |args, ctx| async move { handle_find_single_pin_nets(args, ctx).await }
        ),
        tool!(
            "get_connected_items",
            "Get all wires, labels, and components connected to a given component reference \
             by tracing net connectivity from each of its pins.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "reference": { "type": "string", "description": "Component reference designator (e.g. 'R1')" }
                },
                "required": ["schematic", "reference"]
            }),
            |args, ctx| async move { handle_get_connected_items(args, ctx).await }
        ),
        tool!(
            "check_schematic_overlaps",
            "Find overlapping symbols, visible text, and text over wires. Uses placed body and pin \
             geometry, approximate saved text extents, and reports unresolved symbol bounds.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "tolerance": {
                        "type": "number",
                        "description": "Coordinate tolerance in mm for label collisions and the origin fallback when library geometry is unavailable",
                        "default": 0.5
                    }
                },
                "required": ["schematic"] }),
            |args, ctx| async move { handle_check_overlaps(args, ctx).await }
        ),
    ]
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn handle_list_wires(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let sch = cse::Schematic::load(&sch_path)?;
    let items: Vec<serde_json::Value> = sch.wires.iter()
        .map(|w| json!({ "x1": round6(w.start.0), "y1": round6(w.start.1), "x2": round6(w.end.0), "y2": round6(w.end.1), "uuid": w.uuid }))
        .collect();
    Ok(CallToolResult::json(
        &json!({ "count": items.len(), "wires": items }),
    ))
}

async fn handle_list_nets(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let labels = analysis_net_labels(&tree);
    let nets = analysis_net_graph(&tree, &wires, &labels).names();
    Ok(CallToolResult::json(
        &json!({ "count": nets.len(), "nets": nets }),
    ))
}

async fn handle_list_labels(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let sch = cse::Schematic::load(&sch_path)?;
    let mut items: Vec<serde_json::Value> = Vec::new();
    for l in sch.labels.iter() {
        items.push(json!({ "net": l.text, "type": "NetLabel", "x": round6(l.at.x), "y": round6(l.at.y), "rotation": l.at.rotation.unwrap_or(0.0) }));
    }
    for g in sch.global_labels.iter() {
        items.push(json!({ "net": g.text, "type": "GlobalLabel", "x": round6(g.at.x), "y": round6(g.at.y), "rotation": g.at.rotation.unwrap_or(0.0) }));
    }
    for h in sch.hierarchical_labels.iter() {
        items.push(json!({ "net": h.text, "type": "HierarchicalLabel", "x": round6(h.at.x), "y": round6(h.at.y), "rotation": h.at.rotation.unwrap_or(0.0) }));
    }
    Ok(CallToolResult::json(
        &json!({ "count": items.len(), "labels": items }),
    ))
}

fn net_query_points(
    tree: &konnect_sexp::SexpNode,
    wires: &[Wire],
    labels: &[Label],
    net: &str,
) -> Result<HashSet<(i64, i64)>, CallToolResult> {
    let mut graph = analysis_net_graph(tree, wires, labels);
    let points = graph.points_on_net(&analysis_net_name(net));
    if points.is_empty() {
        let folded = analysis_net_name(net).to_lowercase();
        let suggestions = graph
            .names()
            .into_iter()
            .filter(|name| name.to_lowercase() == folded)
            .collect();
        return Err(CallToolResult::error_kind(
            ToolErrorKind::NotFound {
                net: net.to_owned(),
                suggestions,
            },
            format!("Net '{net}' was not found on the requested schematic sheet"),
        ));
    }
    Ok(points.into_iter().collect())
}

fn net_query_labels(labels: &[Label], net: &str) -> Vec<serde_json::Value> {
    let net = analysis_net_name(net);
    labels
        .iter()
        .filter(|label| label.net == net)
        .map(|label| {
            json!({
                "type": format!("{:?}", label.kind), "x": round6(label.x), "y": round6(label.y),
                "uuid": label.uuid
            })
        })
        .collect()
}

async fn handle_get_net_connections(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let net = match require_str(args, "net") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let labels = analysis_net_labels(&tree);
    let net_pts = match net_query_points(&tree, &wires, &labels, &net) {
        Ok(points) => points,
        Err(error) => return Ok(error),
    };
    let matching = net_query_labels(&labels, &net);
    let mut pins = Vec::new();
    for (instance, placed_pins) in placed_pins_by_reference(&tree) {
        if is_power_symbol_reference(&instance.reference) {
            continue;
        }
        for (pin, transform) in placed_pins {
            let (x, y) = konnect_sexp::schematic::pin_endpoint(&pin, transform);
            if net_pts.contains(&schematic_point(x, y)) {
                pins.push(json!({
                    "reference": instance.reference, "unit": instance.unit,
                    "pin_number": pin.number, "pin_name": pin.name,
                    "electrical_type": pin.electrical_type, "x": round6(x), "y": round6(y)
                }));
            }
        }
    }
    Ok(CallToolResult::json(
        &json!({ "net": net, "label_count": matching.len(), "labels": matching,
            "connected_points": net_pts.len(), "pins": pins }),
    ))
}

async fn handle_get_net_connectivity(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let net = match require_str(args, "net") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let labels = analysis_net_labels(&tree);
    let net_pts = match net_query_points(&tree, &wires, &labels, &net) {
        Ok(points) => points,
        Err(error) => return Ok(error),
    };
    let net_wire_objs: Vec<Wire> = wires
        .into_iter()
        .filter(|w| {
            net_pts.contains(&schematic_point(w.x1, w.y1))
                || net_pts.contains(&schematic_point(w.x2, w.y2))
        })
        .collect();
    let net_wires: Vec<_> = net_wire_objs
        .iter()
        .map(|w| json!({ "x1": round6(w.x1), "y1": round6(w.y1), "x2": round6(w.x2), "y2": round6(w.y2), "uuid": w.uuid }))
        .collect();
    let net_labels = net_query_labels(&labels, &net);
    let t_junctions = konnect_sexp::schematic::find_t_junctions(&net_wire_objs, 0.01);
    Ok(CallToolResult::json(&json!({
        "net": net,
        "wires": net_wires,
        "labels": net_labels,
        "t_junctions": t_junctions.iter().map(|&(x,y)| json!({"x": round6(x), "y": round6(y)})).collect::<Vec<_>>()
    })))
}

async fn handle_get_pin_connections(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let pin_number = match require_str(args, "pin_number") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let (_, tree) = read_schematic(&sch_path)?;
    let instances = extract_symbol_instances(&tree);
    let wires = extract_wires(&tree);
    let labels = analysis_net_labels(&tree);
    if !instances
        .iter()
        .any(|instance| instance.reference == reference)
    {
        return Err(anyhow::anyhow!("Component '{}' not found", reference));
    }
    let pin_ep = placed_pins_by_reference(&tree)
        .into_iter()
        .filter(|(instance, _)| instance.reference == reference)
        .flat_map(|(_, pins)| pins)
        .find(|(pin, _)| pin.number == pin_number)
        .map(|(pin, transform)| konnect_sexp::schematic::pin_endpoint(&pin, transform));
    let (px, py) = match pin_ep {
        Some(ep) => ep,
        None => {
            return Ok(CallToolResult::error(format!(
                "Pin '{}' not found on '{}'",
                pin_number, reference
            )))
        }
    };
    let mut g = analysis_net_graph(&tree, &wires, &labels);
    Ok(CallToolResult::json(
        &json!({ "reference": reference, "pin": pin_number, "pin_x": px, "pin_y": py, "net": g.net_at(px, py) }),
    ))
}

async fn handle_get_component_nets(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let (_, tree) = read_schematic(&sch_path)?;
    let instances = extract_symbol_instances(&tree);
    let wires = extract_wires(&tree);
    let labels = analysis_net_labels(&tree);
    if !instances
        .iter()
        .any(|instance| instance.reference == reference)
    {
        return Err(anyhow::anyhow!("Component '{}' not found", reference));
    }
    let mut g = analysis_net_graph(&tree, &wires, &labels);
    let pins: Vec<serde_json::Value> = placed_pins_by_reference(&tree)
        .into_iter()
        .filter(|(instance, _)| instance.reference == reference)
        .flat_map(|(_, pins)| pins)
        .map(|(pin, transform)| {
            let (px, py) = konnect_sexp::schematic::pin_endpoint(&pin, transform);
            json!({ "pin": pin.number, "name": pin.name, "x": round6(px), "y": round6(py), "net": g.net_at(px, py) })
        })
        .collect();
    Ok(CallToolResult::json(
        &json!({ "reference": reference, "pins": pins }),
    ))
}

async fn handle_get_net_components(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let net = match require_str(args, "net") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let labels = analysis_net_labels(&tree);
    let mut g = analysis_net_graph(&tree, &wires, &labels);
    let net_pts: HashSet<(i64, i64)> = g
        .points_on_net(&analysis_net_name(&net))
        .into_iter()
        .collect();
    let result: Vec<serde_json::Value> = placed_pins_by_reference(&tree)
        .into_iter()
        .filter_map(|(instance, pins)| {
            let connected: Vec<_> = pins
                .into_iter()
                .filter_map(|(pin, transform)| {
                    let (px, py) = konnect_sexp::schematic::pin_endpoint(&pin, transform);
                    if net_pts.contains(&schematic_point(px, py)) {
                        Some(json!({ "pin": pin.number, "name": pin.name }))
                    } else {
                        None
                    }
                })
                .collect();
            if connected.is_empty() {
                None
            } else {
                Some(json!({ "reference": instance.reference, "value": instance.value, "pins": connected }))
            }
        })
        .collect();
    Ok(CallToolResult::json(
        &json!({ "net": net, "components": result }),
    ))
}

async fn handle_trace_from_point(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let x = match require_f64(args, "x") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let y = match require_f64(args, "y") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // A zero or negative tolerance makes every coincidence test below answer
    // "nothing here" while the net graph, which resolves at its own fixed
    // tolerance, still names a net — a successful response asserting a net at a
    // point it also reports as bare. Refused rather than answered, as
    // `find_orphan_items` refuses it.
    let tol = match opt_positive_f64(args, "tolerance") {
        Ok(value) => value.unwrap_or(0.05),
        Err(e) => return Ok(e),
    };
    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let buses = extract_buses(&tree);
    let labels = analysis_net_labels(&tree);
    let mut g = analysis_net_graph(&tree, &wires, &labels);
    let on_wire: Vec<_> = wires
        .iter()
        .filter(|w| {
            points_coincident(x, y, w.x1, w.y1, tol)
                || points_coincident(x, y, w.x2, w.y2, tol)
                || point_on_segment(x, y, w.x1, w.y1, w.x2, w.y2, tol)
        })
        .map(|w| json!({ "x1": round6(w.x1), "y1": round6(w.y1), "x2": round6(w.x2), "y2": round6(w.y2) }))
        .collect();
    let at_label: Vec<_> = labels
        .iter()
        .filter(|l| points_coincident(x, y, l.x, l.y, tol))
        .map(|l| json!({ "net": l.net, "type": format!("{:?}", l.kind) }))
        .collect();
    // A pin and a junction dot sit on a point as readily as a wire or a label,
    // and this is the tool whose whole promise is what is there (#539). Both
    // come from `ConnectivityIndex`, which owns what attaches at a point: a
    // second scan here is exactly how the connectivity tools drifted apart
    // before, and the index is also unit-aware, so a multi-unit symbol
    // contributes only the unit actually placed (#35).
    let index = ConnectivityIndex::build(&tree, &wires, &buses, &labels, tol);
    let detail = index.point_detail(x, y);
    let at_pin: Vec<_> = detail
        .pins
        .iter()
        .map(|placed| {
            json!({
                "reference": placed.reference,
                "pin": placed.pin.number,
                "pin_name": placed.pin.name,
                "electrical_type": placed.pin.electrical_type,
                "x": round6(placed.at.0),
                "y": round6(placed.at.1)
            })
        })
        .collect();
    let at_junction: Vec<_> = detail
        .junctions
        .iter()
        .map(|&(jx, jy)| json!({ "x": round6(jx), "y": round6(jy) }))
        .collect();
    Ok(CallToolResult::json(&json!({
        "x": round6(x),
        "y": round6(y),
        "net": g.net_at(x, y),
        "wires_here": on_wire,
        "labels_here": at_label,
        "pins_here": at_pin,
        "junctions_here": at_junction
    })))
}

async fn handle_find_orphan_items(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let tolerance = opt_f64(args, "tolerance").unwrap_or(0.05);
    if !tolerance.is_finite() || tolerance <= 0.0 {
        return Ok(CallToolResult::error(
            "Invalid argument 'tolerance': must be finite and positive",
        ));
    }

    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let buses = extract_buses(&tree);
    let labels = analysis_net_labels(&tree);
    let index = ConnectivityIndex::for_electrical_analysis(&tree, &wires, &buses, &labels);
    let proximity = ConnectivityIndex::build(&tree, &wires, &buses, &labels, tolerance);
    let mut near_misses = Vec::new();
    let mut all = Vec::new();
    let mut report = |item: serde_json::Value, is_near: bool| {
        if is_near {
            near_misses.push(item.clone());
        }
        all.push(item);
    };

    // A wire end is dangling only when nothing terminates it. Ending on a
    // component or hierarchical sheet pin is the normal case.
    for (x, y, wire_uuid) in index.floating_wire_ends() {
        report(
            json!({
                "type": "dangling_wire_end",
                "x": round6(x),
                "y": round6(y),
                "wire_uuid": wire_uuid
            }),
            proximity.terminates_wire_end(x, y),
        );
    }

    // Labels connect anywhere along a wire, not only at its endpoint, or
    // directly on a bare symbol pin. Only real labels are reported: a power
    // symbol that connects to nothing is an unconnected pin, below, and
    // reporting it twice would be a second answer to one question.
    for label in index
        .labels()
        .iter()
        .filter(|label| label.kind != LabelKind::PowerSymbol)
    {
        if !index.on_wire(label.x, label.y)
            && !index.on_bus(label.x, label.y)
            && !index.has_pin(label.x, label.y)
        {
            report(
                json!({
                    "type": "floating_label",
                    "net": label.net,
                    "x": round6(label.x),
                    "y": round6(label.y)
                }),
                proximity.on_wire(label.x, label.y)
                    || proximity.on_bus(label.x, label.y)
                    || proximity.has_pin(label.x, label.y),
            );
        }
    }

    // Report the unconnected pins promised by the tool description. A pin
    // sitting mid-wire connects only through a junction dot (#104).
    for placed in index.placed_pins() {
        let (x, y) = placed.at;
        if placed.pin.electrical_type == "no_connect" || index.has_no_connect(x, y) {
            continue;
        }
        if !index.attaches_pin(x, y) {
            let missed_pin = json!({
                "type": "unconnected_pin",
                "reference": placed.reference,
                "pin": placed.pin.number,
                "pin_name": placed.pin.name,
                "x": round6(x),
                "y": round6(y)
            });
            report(missed_pin, proximity.attaches_pin(x, y));
        }
    }

    Ok(CallToolResult::json(&json!({
        "orphan_count": all.len(),
        "orphans": all,
        "near_misses": near_misses,
        "tolerance": tolerance
    })))
}

async fn handle_find_shorted_nets(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let labels = analysis_net_labels(&tree);
    let mut g = analysis_net_graph(&tree, &wires, &labels);
    let mut root_nets: HashMap<(i64, i64), Vec<&str>> = HashMap::new();
    for (root, label) in label_roots(&mut g, &labels) {
        root_nets.entry(root).or_default().push(label.net.as_str());
    }
    let shorts: Vec<serde_json::Value> = root_nets
        .into_values()
        .filter_map(|mut nets| {
            nets.sort();
            nets.dedup();
            if nets.len() > 1 {
                Some(json!({ "shorted_nets": nets }))
            } else {
                None
            }
        })
        .collect();
    Ok(CallToolResult::json(
        &json!({ "short_count": shorts.len(), "shorts": shorts }),
    ))
}

/// A point that counts as reaching a net.
enum Connection<'a> {
    ComponentPin {
        reference: &'a str,
        pin: &'a LibPin,
        x: f64,
        y: f64,
    },
    /// A hierarchical sheet pin: whatever the net meets on the other side.
    SheetPin { x: f64, y: f64 },
}

impl Connection<'_> {
    fn to_json(&self) -> serde_json::Value {
        match self {
            Connection::ComponentPin {
                reference,
                pin,
                x,
                y,
            } => json!({
                "type": "component_pin",
                "reference": reference,
                "pin": pin.number,
                "pin_name": pin.name,
                "x": round6(*x),
                "y": round6(*y)
            }),
            Connection::SheetPin { x, y } => {
                json!({ "type": "sheet_pin", "x": round6(*x), "y": round6(*y) })
            }
        }
    }
}

/// Whether a label of this kind can carry its net off the sheet being read.
///
/// A local `NetLabel` cannot: it names a net within one sheet, so a count taken
/// from that sheet is the whole answer. The other three can, and a reader has
/// to know which it is holding. `GlobalLabel` and a power symbol — which KiCAD
/// makes a global label out of — join every same-named net in the project;
/// `HierarchicalLabel` continues through the parent's sheet pin.
fn reaches_beyond_this_sheet(kind: &LabelKind) -> bool {
    match kind {
        LabelKind::NetLabel => false,
        LabelKind::GlobalLabel | LabelKind::HierarchicalLabel | LabelKind::PowerSymbol => true,
    }
}

/// Nets that reach at most one pin.
///
/// Zero counts as well as one: a label whose net reaches nothing is the orphan
/// label or the stub left by a deleted component that the review skill sends
/// this tool looking for, and `find_orphan_items` reports the dangling wire end
/// without ever naming the net. `pin_count` tells the two apart.
///
/// This counted *label instances* per net name, so every net drawn the ordinary
/// way — one label on a wire that reaches two or more pins — was reported, and
/// the defect the tool advertises passed through unreported as soon as its net
/// carried a second label. The count comes from the shared net graph now, and
/// the label count stays beside it as its own field: a net with no label at all
/// is a different smell.
///
/// A power symbol's own pin is not a connection here, though it is one to
/// `find_orphan_items`, which reports an unwired `#PWR01`. The divergence is
/// deliberate: the rail that reaches exactly one component pin is what this
/// tool is for, and counting the symbol that named it would hide every one of
/// them.
///
/// The answer is per sheet, so a net named by a global or hierarchical label
/// may well continue on another one, and the report has to say so rather than
/// leave the caller to infer it. `cross_sheet_unverified` is that statement:
/// true when any label naming the net can carry it off this sheet, which is as
/// far as a single-sheet reader can go. Reporting the net anyway is deliberate
/// — a rail that reaches one pin here is worth showing — but the flag marks it
/// as a lead rather than a finding.
///
/// # Response compatibility
///
/// Every field this tool used to return is still returned and still means the
/// same thing: `single_pin_net_count`, and per net `net`, `x`, `y`, and `type`.
/// `type` remains the kind of the *first* label found, so a consumer matching
/// on it is unaffected. It is also why the other two fields exist: local labels
/// are extracted first, so a net carrying both a local and a global label
/// reports `NetLabel` and says nothing about the global one. `label_types`
/// carries every distinct kind, sorted; `cross_sheet_unverified` is the same
/// evidence as one boolean.
///
/// The additive fields are `label_types`, `cross_sheet_unverified`,
/// `pin_count`, `label_count`, and `pins`.
///
/// What *does* change is which nets appear, because that was the defect: the
/// membership rule went from "exactly one label instance names it" to "the net
/// reaches at most one pin". A consumer reading `single_pin_net_count` as a
/// defect count gets a smaller, truer number and needs no migration; one that
/// had learned to ignore this tool's noise can stop. No consumer can keep the
/// old set, since it was wrong in both directions.
async fn handle_find_single_pin_nets(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let labels = analysis_net_labels(&tree);
    let mut graph = analysis_net_graph(&tree, &wires, &labels);

    // Every connection point, under the graph root that carries it. A sheet
    // holds hundreds and reports a handful, so nothing is rendered until a net
    // qualifies.
    let placed = placed_pins_by_reference(&tree);
    let mut pins_by_root: HashMap<(i64, i64), Vec<Connection>> = HashMap::new();
    for (instance, pins) in &placed {
        if is_power_symbol_reference(&instance.reference) {
            continue;
        }
        for (pin, transform) in pins {
            let (x, y) = konnect_sexp::schematic::pin_endpoint(pin, *transform);
            pins_by_root
                .entry(graph.root_at(x, y))
                .or_default()
                .push(Connection::ComponentPin {
                    reference: &instance.reference,
                    pin,
                    x,
                    y,
                });
        }
    }
    for (x, y) in extract_sheet_pins(&tree) {
        pins_by_root
            .entry(graph.root_at(x, y))
            .or_default()
            .push(Connection::SheetPin { x, y });
    }

    // Each net name, the roots its labels sit on, and the first label to name
    // it. Two labels of one name on segments that never meet are still one net
    // to KiCAD, so the roots pool.
    struct NamedNet<'a> {
        roots: HashSet<(i64, i64)>,
        label_count: usize,
        first_label: &'a Label,
        /// Every kind that named this net, not just the first. Sorted, so the
        /// field reads the same on two runs over one sheet.
        kinds: BTreeSet<String>,
        cross_sheet_unverified: bool,
    }
    let mut by_net: HashMap<&str, NamedNet> = HashMap::new();
    for (root, label) in label_roots(&mut graph, &labels) {
        let named = by_net
            .entry(label.net.as_str())
            .or_insert_with(|| NamedNet {
                roots: HashSet::new(),
                label_count: 0,
                first_label: label,
                kinds: BTreeSet::new(),
                cross_sheet_unverified: false,
            });
        named.roots.insert(root);
        named.label_count += 1;
        named.kinds.insert(format!("{:?}", label.kind));
        named.cross_sheet_unverified |= reaches_beyond_this_sheet(&label.kind);
    }

    // HashMap order is not an answer: two runs of one binary over one sheet
    // would report the same nets in different orders.
    let mut by_net: Vec<(&str, NamedNet)> = by_net.into_iter().collect();
    by_net.sort_by_key(|(net, _)| *net);

    let singles: Vec<serde_json::Value> = by_net
        .iter()
        .filter_map(|(net, named)| {
            let mut on_net = named
                .roots
                .iter()
                .filter_map(|root| pins_by_root.get(root))
                .flatten();
            let reached = on_net.next();
            if on_net.next().is_some() {
                return None;
            }
            let pins: Vec<serde_json::Value> =
                reached.map(Connection::to_json).into_iter().collect();
            Some(json!({
                "net": net,
                "x": round6(named.first_label.x),
                "y": round6(named.first_label.y),
                "type": format!("{:?}", named.first_label.kind),
                "label_types": named.kinds,
                "cross_sheet_unverified": named.cross_sheet_unverified,
                "pin_count": pins.len(),
                "label_count": named.label_count,
                "pins": pins
            }))
        })
        .collect();

    Ok(CallToolResult::json(
        &json!({ "single_pin_net_count": singles.len(), "nets": singles }),
    ))
}

async fn handle_get_connected_items(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };

    let (_, tree) = read_schematic(&sch_path)?;
    let instances = extract_symbol_instances(&tree);
    let wires = extract_wires(&tree);
    let labels = analysis_net_labels(&tree);
    if !instances
        .iter()
        .any(|instance| instance.reference == reference)
    {
        return Ok(CallToolResult::error(format!(
            "Component '{}' not found",
            reference
        )));
    }
    let placed_pins = placed_pins_by_reference(&tree);
    let mut g = analysis_net_graph(&tree, &wires, &labels);

    // The nets this component's pins are on, held by identity: a net can carry
    // more than one name, and one can carry none at all. Keying this by name
    // dropped the labels spelling a net its other way, and dropped an unnamed
    // net's items entirely.
    let mut connected_roots: HashSet<(i64, i64)> = HashSet::new();
    let mut connected_nets: BTreeSet<String> = BTreeSet::new();
    for (_, pins) in placed_pins
        .iter()
        .filter(|(instance, _)| instance.reference == reference)
    {
        for (pin, transform) in pins {
            let (px, py) = konnect_sexp::schematic::pin_endpoint(pin, *transform);
            let root = g.root_at(px, py);
            connected_roots.insert(root);
            if let Some(net) = g.name_of_root(root) {
                connected_nets.insert(net);
            }
        }
    }

    // Find all wires, labels, and components on those nets
    let all_net_pts: HashSet<(i64, i64)> =
        g.points_on_roots(&connected_roots).into_iter().collect();

    let connected_wires: Vec<serde_json::Value> = wires
        .iter()
        .filter(|w| {
            all_net_pts.contains(&schematic_point(w.x1, w.y1)) || all_net_pts.contains(&schematic_point(w.x2, w.y2))
        })
        .map(|w| json!({ "x1": round6(w.x1), "y1": round6(w.y1), "x2": round6(w.x2), "y2": round6(w.y2), "uuid": w.uuid }))
        .collect();

    let connected_labels: Vec<serde_json::Value> = label_roots(&mut g, &labels)
        .into_iter()
        .filter(|(root, _)| connected_roots.contains(root))
        .map(|(_, l)| json!({ "net": l.net, "type": format!("{:?}", l.kind), "x": round6(l.x), "y": round6(l.y) }))
        .collect();

    // Find other components on the same nets (excluding the queried one)
    let connected_components: Vec<serde_json::Value> = placed_pins
        .iter()
        .filter(|(instance, _)| instance.reference != reference)
        .filter_map(|(instance, pins)| {
            let matching_pins: Vec<_> = pins
                .iter()
                .filter_map(|(pin, transform)| {
                    let (px, py) = konnect_sexp::schematic::pin_endpoint(pin, *transform);
                    if all_net_pts.contains(&schematic_point(px, py)) {
                        Some(json!({ "pin": pin.number, "name": pin.name }))
                    } else {
                        None
                    }
                })
                .collect();
            if matching_pins.is_empty() {
                None
            } else {
                Some(json!({ "reference": instance.reference, "value": instance.value, "connected_pins": matching_pins }))
            }
        })
        .collect();

    Ok(CallToolResult::json(&json!({
        "reference": reference,
        "nets": connected_nets,
        "connected_wires": connected_wires.len(),
        "wires": connected_wires,
        "labels": connected_labels,
        "connected_components": connected_components
    })))
}

async fn handle_check_overlaps(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let tol = opt_f64(args, "tolerance").unwrap_or(0.5);
    if !tol.is_finite() || tol < 0.0 {
        return Ok(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::InvalidArgument {
                field: "tolerance".to_string(),
                reason: "must be a finite, non-negative number".to_string(),
            },
            "Argument 'tolerance' must be a finite, non-negative number.",
        ));
    }
    let (_, tree) = read_schematic(&sch_path)?;
    Ok(CallToolResult::json(&super::sch_overlap::find_overlaps(
        &tree, tol,
    )))
}

#[cfg(test)]
mod placement_overlap_tests {
    use super::*;
    use crate::router::ToolRouter;
    use crate::tools::{ServerConfig, ToolContext};
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        ToolContext::new(
            ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            Arc::new(ToolRouter::new()),
        )
    }

    /// Retained graphic and pin nodes from KiCad 10's stock Device:R.
    const DEVICE_R: &str = r#"		(symbol "Device:R"
			(symbol "R_0_1"
				(rectangle
					(start -1.016 -2.54)
					(end 1.016 2.54)
					(stroke (width 0.254) (type default))
					(fill (type none))
				)
			)
			(symbol "R_1_1"
				(pin passive line
					(at 0 3.81 270)
					(length 1.27)
					(name "" (effects (font (size 1.27 1.27))))
					(number "1" (effects (font (size 1.27 1.27))))
				)
				(pin passive line
					(at 0 -3.81 90)
					(length 1.27)
					(name "" (effects (font (size 1.27 1.27))))
					(number "2" (effects (font (size 1.27 1.27))))
				)
			)
		)
"#;

    fn schematic(placements: &[(&str, f64, f64)]) -> String {
        let instances = placements
            .iter()
            .map(|(reference, x, y)| {
                format!(
                    "\t(symbol\n\t\t(lib_id \"Device:R\")\n\t\t(at {x} {y} 0)\n\t\t(unit 1)\n\t\t(uuid \"{reference}\")\n\t\t(property \"Reference\" \"{reference}\")\n\t\t(property \"Value\" \"10k\")\n\t)\n"
                )
            })
            .collect::<String>();
        format!(
            "(kicad_sch\n\t(version 20260206)\n\t(generator \"eeschema\")\n\t(lib_symbols\n{DEVICE_R}\t)\n{instances})\n"
        )
    }

    fn response_json(result: &CallToolResult) -> serde_json::Value {
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text result");
        };
        serde_json::from_str(text).unwrap()
    }

    async fn overlaps(source: &str) -> serde_json::Value {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("overlap.kicad_sch");
        std::fs::write(&path, source).unwrap();
        let result =
            handle_check_overlaps(&json!({"schematic": path.to_string_lossy()}), &test_ctx())
                .await
                .unwrap();
        response_json(&result)
    }

    #[tokio::test]
    async fn distinct_origins_with_intersecting_symbol_bodies_are_reported() {
        let result = overlaps(&schematic(&[
            ("R1", 100.0, 50.0),
            ("R2", 101.0, 50.0),
            ("R3", 110.0, 50.0),
        ]))
        .await;

        assert_eq!(result["bounds_resolved"], 3);
        assert_eq!(result["bounds_unresolved"], json!([]));
        assert_eq!(result["overlap_count"], 1);
        assert_eq!(result["overlaps"][0]["a"], "R1");
        assert_eq!(result["overlaps"][0]["b"], "R2");
        assert_eq!(result["overlaps"][0]["detection"], "symbol_geometry");
        assert!(result["overlaps"][0]["overlap_x_mm"].as_f64().unwrap() > 1.0);
    }

    #[tokio::test]
    async fn symbols_whose_bounds_only_touch_are_not_collisions() {
        let result = overlaps(&schematic(&[("R1", 100.0, 50.0), ("R2", 102.032, 50.0)])).await;

        assert_eq!(result["overlap_count"], 0, "{result}");
    }

    const NATIVE: &str = include_str!("../../tests/fixtures/overlaps_kicad10.kicad_sch");

    #[tokio::test]
    async fn native_label_extents_and_power_pin_tip_are_distinguished() {
        let result = overlaps(NATIVE).await;
        let findings = result["overlaps"].as_array().unwrap();
        assert_eq!(findings.len(), 1, "{result}");
        assert_eq!(findings[0]["type"], "label_overlap");
        let uuids = [&findings[0]["uuid_a"], &findings[0]["uuid_b"]];
        assert!(uuids.contains(&&json!("fc0cc965-47c7-4935-964f-b2936fb5d4cf")));
        assert!(uuids.contains(&&json!("0fa485e0-be78-49a0-8d1e-fecd5a02403a")));

        let moved = NATIVE.replacen("(at 96.19 100 0)", "(at 100 99.5 0)", 1);
        let result = overlaps(&moved).await;
        let findings = result["overlaps"].as_array().unwrap();
        let collision = findings
            .iter()
            .find(|finding| finding["type"] == "component_overlap")
            .unwrap();
        assert_eq!(collision["uuid_a"], "0bb65184-85ad-4de5-9aa6-9c6c8c3e92bc");
        assert_eq!(collision["uuid_b"], "b88ea8a8-1554-4c53-94d5-5e6a70a26915");
    }

    #[tokio::test]
    async fn conflicting_opposite_labels_still_use_anchor_tolerance() {
        use super::tool_call_support::call;

        let source = r#"(kicad_sch
            (label "RIGHT" (at 100 100 0)
                (effects (font (size 1.27 1.27)) (justify left bottom)) (uuid "right"))
            (label "LEFT" (at 99.8 100 180)
                (effects (font (size 1.27 1.27)) (justify right bottom)) (uuid "left")))"#;
        let result = overlaps(source).await;
        assert_eq!(result["overlap_count"], 1);
        assert_eq!(result["overlaps"][0]["uuid_a"], "right");
        assert_eq!(result["overlaps"][0]["uuid_b"], "left");

        let result = call(
            source,
            "check_schematic_overlaps",
            json!({ "tolerance": 0.1 }),
        )
        .await;
        assert_eq!(result["overlap_count"], 0);
    }

    #[tokio::test]
    async fn oriented_labels_and_mirrored_symbol_fields_use_saved_layout() {
        let extras = r#"
  (wire (pts (xy 133 128) (xy 133 132)) (uuid "global-cross"))
  (wire (pts (xy 173 120) (xy 177 120)) (uuid "hier-cross"))
  (text "X" (at 101.2 95 0) (effects (font (size 0.6 0.6))) (uuid "field-probe"))
"#;
        let modified = format!("{}{extras})", NATIVE.trim_end().strip_suffix(')').unwrap());
        let result = overlaps(&modified).await;
        let findings = result["overlaps"].as_array().unwrap();
        for (text, wire) in [
            ("984f10b4-09d0-4215-bb1e-64ce340ee3bf", "global-cross"),
            ("bd75fa5e-7bb2-4c8d-b77e-8b5af48ef288", "hier-cross"),
        ] {
            assert!(
                findings.iter().any(|item| {
                    item["type"] == "text_wire_overlap"
                        && item["uuid_a"] == text
                        && item["uuid_b"] == wire
                }),
                "{result}"
            );
        }
        assert!(
            findings.iter().any(|item| {
                item["type"] == "text_overlap"
                    && [item["uuid_a"].as_str(), item["uuid_b"].as_str()]
                        .contains(&Some("field-probe"))
                    && [item["uuid_a"].as_str(), item["uuid_b"].as_str()]
                        .contains(&Some("b88ea8a8-1554-4c53-94d5-5e6a70a26915"))
            }),
            "{result}"
        );
    }

    #[tokio::test]
    async fn text_reports_sheet_owners_and_other_bodies_but_skips_hidden_fields() {
        let source = schematic(&[("R1", 100.0, 50.0), ("R2", 110.0, 50.0)])
            .replacen(
                "(property \"Value\" \"10k\")",
                "(property \"Value\" \"10k\" (at 110 50 0) (effects (font (size 1.27 1.27))))",
                1,
            )
            .replacen(
                "(property \"Reference\" \"R2\")",
                "(property \"Reference\" \"HIDDEN\" (at 130 50 0) (hide yes) (effects (font (size 1.27 1.27))))",
                1,
            );
        let extras = r#"
  (sheet (at 130 51) (size 10 10) (uuid "sheet")
    (property "Sheetname" "CHILD" (at 130 50 0) (effects (font (size 1.27 1.27)))))
  (text "NOTE" (at 130 50 0) (effects (font (size 1.27 1.27))))
"#;
        let result = overlaps(&format!(
            "{}{extras})",
            source.trim_end().strip_suffix(')').unwrap()
        ))
        .await;
        let findings = result["overlaps"].as_array().unwrap();
        assert_eq!(findings.len(), 2, "{result}");
        assert!(
            findings.iter().any(|item| {
                item["type"] == "text_body_overlap"
                    && item["uuid_a"] == "R1"
                    && item["uuid_b"] == "R2"
            }),
            "{result}"
        );
        assert!(
            findings.iter().any(|item| {
                item["type"] == "text_overlap"
                    && [item["uuid_a"].as_str(), item["uuid_b"].as_str()].contains(&Some("sheet"))
                    && (item["uuid_a"].is_null() || item["uuid_b"].is_null())
            }),
            "{result}"
        );
    }

    #[tokio::test]
    async fn invalid_overlap_tolerance_is_a_named_argument_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("overlap.kicad_sch");
        std::fs::write(&path, schematic(&[("R1", 100.0, 50.0)])).unwrap();
        let result = handle_check_overlaps(
            &json!({"schematic": path.to_string_lossy(), "tolerance": -0.1}),
            &test_ctx(),
        )
        .await
        .unwrap();
        let result = response_json(&result);

        assert_eq!(result["error"]["kind"], "invalid_argument");
        assert_eq!(result["error"]["field"], "tolerance");
    }
}

#[cfg(test)]
#[cfg(test)]
mod orphan_item_tests {
    use super::*;
    use crate::tools::ServerConfig;
    use std::io::Write;
    use std::sync::Arc;

    /// Run the registered tool against a temporary schematic, exactly as the
    /// MCP dispatch layer does after selecting its `ToolDef`.
    async fn call_result(schematic: &str, mut args: serde_json::Value) -> CallToolResult {
        let mut file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        file.write_all(schematic.as_bytes()).unwrap();
        file.flush().unwrap();

        args["schematic"] = json!(file.path().to_str().unwrap());
        let definition = tools()
            .into_iter()
            .find(|tool| tool.name == "find_orphan_items")
            .unwrap();
        let context = ToolContext::new(
            ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            Arc::new(crate::router::ToolRouter::new()),
        );
        (definition.handler)(&args, Arc::new(context))
            .await
            .unwrap()
    }

    async fn call(schematic: &str, args: serde_json::Value) -> serde_json::Value {
        let result = call_result(schematic, args).await;
        assert!(!result.is_error, "find_orphan_items failed");
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        serde_json::from_str(text).unwrap()
    }

    /// One wire from (90,100) to a zero-length pin of U1 at (100,100), a `SIG`
    /// label mid-segment, a stray `ORPHAN` label, and U2 with nothing on its pin.
    fn schematic(extra: &str) -> String {
        format!(
            r#"(kicad_sch
  (version 20260306)
  (generator "eeschema")
  (uuid "root")
  (lib_symbols
    (symbol "Test:P"
      (symbol "P_1_1"
        (pin passive line (at 0 0 0) (length 0) (name "A") (number "1"))
      )
    )
  )
  (wire (pts (xy 90 100) (xy 100 100)) (uuid "w1"))
  (label "SIG" (at 95 100 0))
  (label "ORPHAN" (at 200 200 0))
  (symbol (lib_id "Test:P") (at 100 100 0) (unit 1) (uuid "u1")
    (property "Reference" "U1" (at 100 100 0))
    (property "Value" "P" (at 100 100 0))
  )
  (symbol (lib_id "Test:P") (at 150 150 0) (unit 1) (uuid "u2")
    (property "Reference" "U2" (at 150 150 0))
    (property "Value" "P" (at 150 150 0))
  )
{extra}  (sheet_instances (path "/" (page "1")))
)
"#
        )
    }

    async fn orphans(extra: &str) -> Vec<serde_json::Value> {
        let body = call(&schematic(extra), json!({})).await;
        body["orphans"].as_array().unwrap().clone()
    }

    fn of_type<'a>(items: &'a [serde_json::Value], kind: &str) -> Vec<&'a serde_json::Value> {
        items.iter().filter(|item| item["type"] == kind).collect()
    }

    #[tokio::test]
    async fn a_wire_ending_on_a_pin_is_not_dangling() {
        let items = orphans("").await;
        let dangling = of_type(&items, "dangling_wire_end");
        assert_eq!(dangling.len(), 1, "only the free end counts: {items:?}");
        assert_eq!(dangling[0]["x"], 90.0);
        assert_eq!(dangling[0]["wire_uuid"], "w1");
    }

    #[tokio::test]
    async fn a_label_mid_segment_is_not_floating() {
        let items = orphans("").await;
        let floating = of_type(&items, "floating_label");
        assert_eq!(floating.len(), 1, "only ORPHAN floats: {items:?}");
        assert_eq!(floating[0]["net"], "ORPHAN");
    }

    #[tokio::test]
    async fn a_pin_with_nothing_on_it_is_reported() {
        let items = orphans("").await;
        let pins = of_type(&items, "unconnected_pin");
        assert_eq!(pins.len(), 1, "U1's pin is wired: {items:?}");
        assert_eq!(pins[0]["reference"], "U2");
        assert_eq!(pins[0]["pin"], "1");
        assert_eq!(pins[0]["pin_name"], "A");
    }

    #[tokio::test]
    async fn an_off_grid_endpoint_is_reported_as_unconnected_and_as_a_near_miss() {
        // KiCad 10.0.8 leaves a pin unconnected when the saved wire stops
        // 0.03 mm short, inside the previous default proximity tolerance.
        let near = schematic("").replace("(xy 100 100)", "(xy 99.97 100)");
        let result = call(&near, json!({})).await;
        let orphans = result["orphans"].as_array().unwrap();
        assert!(of_type(orphans, "unconnected_pin")
            .iter()
            .any(|pin| pin["reference"] == "U1"));
        assert!(result["near_misses"]
            .as_array()
            .unwrap()
            .iter()
            .any(|pin| pin["reference"] == "U1"));
    }

    /// The reported #249 case: a label directly on a pin without a wire is a
    /// legal KiCAD connection and connects both items.
    #[tokio::test]
    async fn a_label_on_a_bare_pin_connects_both_items() {
        let items = orphans("  (label \"NC_SIG\" (at 150 150 0))\n").await;
        let floating = of_type(&items, "floating_label");
        assert_eq!(floating.len(), 1, "NC_SIG is on U2's pin: {items:?}");
        assert_eq!(floating[0]["net"], "ORPHAN");
        assert!(
            of_type(&items, "unconnected_pin").is_empty(),
            "the label connects U2: {items:?}"
        );
    }

    #[tokio::test]
    async fn a_no_connect_flag_exempts_its_pin() {
        let items = orphans("  (no_connect (at 150 150) (uuid \"nc1\"))\n").await;
        assert!(
            of_type(&items, "unconnected_pin").is_empty(),
            "no-connect covers U2: {items:?}"
        );
    }

    #[tokio::test]
    async fn an_intrinsically_no_connect_pin_is_exempt() {
        let no_connect_pin = schematic("").replace(
            "(pin passive line (at 0 0 0)",
            "(pin no_connect line (at 0 0 0)",
        );
        let body = call(&no_connect_pin, json!({})).await;
        let items = body["orphans"].as_array().unwrap();
        assert!(
            of_type(items, "unconnected_pin").is_empty(),
            "library no-connect pins are intentional: {items:?}"
        );
    }

    /// A multi-unit symbol must contribute only the pins from the placed unit.
    #[tokio::test]
    async fn another_unit_does_not_contribute_a_phantom_pin() {
        let schematic = r#"(kicad_sch
  (version 20260306)
  (generator "eeschema")
  (uuid "root")
  (lib_symbols
    (symbol "Test:D"
      (symbol "D_1_1"
        (pin passive line (at 0 0 0) (length 0) (name "A") (number "1"))
      )
      (symbol "D_2_1"
        (pin passive line (at 10 0 0) (length 0) (name "B") (number "2"))
      )
    )
  )
  (wire (pts (xy 90 100) (xy 100 100)) (uuid "w1"))
  (label "SIG" (at 90 100 0))
  (symbol (lib_id "Test:D") (at 100 100 0) (unit 1) (uuid "u1")
    (property "Reference" "U1" (at 100 100 0))
    (property "Value" "D" (at 100 100 0))
  )
  (sheet_instances (path "/" (page "1")))
)
"#;
        let body = call(schematic, json!({})).await;
        assert_eq!(body["orphan_count"], 0, "unit 2 is not placed here: {body}");
    }

    #[tokio::test]
    async fn a_wire_ending_on_a_sheet_pin_is_not_dangling() {
        let sheet = r#"  (sheet (at 80 95) (size 10 10) (uuid "s1")
    (property "Sheetname" "sub" (at 80 95 0))
    (property "Sheetfile" "sub.kicad_sch" (at 80 95 0))
    (pin "SIG" input (at 90 100 180) (uuid "sp1"))
  )
"#;
        let items = orphans(sheet).await;
        assert!(
            of_type(&items, "dangling_wire_end").is_empty(),
            "both ends terminate: {items:?}"
        );
    }

    #[tokio::test]
    async fn tolerance_must_be_positive() {
        for tolerance in [0.0, -0.05] {
            let result = call_result(&schematic(""), json!({ "tolerance": tolerance })).await;
            assert!(result.is_error, "accepted tolerance {tolerance}");
        }
    }
}

#[cfg(test)]
mod tool_call_support {
    use super::*;
    use crate::tools::ServerConfig;
    use std::io::Write;
    use std::sync::Arc;

    /// Run a tool by name against a temp file holding `sch`, exactly as the MCP
    /// dispatch layer does after selecting its `ToolDef`. Returns the result as
    /// it comes back, so a refusal can be asserted as a refusal.
    pub(super) async fn call_result(
        sch: &str,
        tool: &str,
        mut args: serde_json::Value,
    ) -> CallToolResult {
        let mut f = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        f.write_all(sch.as_bytes()).unwrap();
        f.flush().unwrap();

        args["schematic"] = json!(f.path().to_str().unwrap());
        let def = tools().into_iter().find(|t| t.name == tool).unwrap();
        let ctx = ToolContext::new(
            ServerConfig::default(),
            Arc::new(crate::router::ToolRouter::new()),
        );
        (def.handler)(&args, Arc::new(ctx)).await.unwrap()
    }

    /// The body of a call that must have succeeded.
    pub(super) async fn call(sch: &str, tool: &str, args: serde_json::Value) -> serde_json::Value {
        let result = call_result(sch, tool, args).await;
        assert!(!result.is_error, "{tool} failed");
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        serde_json::from_str(text).unwrap()
    }
}

#[cfg(test)]
mod power_symbol_net_tests {
    use super::tool_call_support::call;
    use super::*;

    /// R1 with pin 1 on a `SIG` label and pin 2 wired down to a `power:GND`
    /// symbol. The rail is what a label-only net graph loses.
    const SCH: &str = include_str!("../../tests/fixtures/power_rail.kicad_sch");

    /// The S-expression path: a pin reached only through a power symbol used to
    /// report no net at all.
    #[tokio::test]
    async fn a_pin_on_a_rail_reports_it() {
        let s = call(
            SCH,
            "get_pin_connections",
            json!({ "reference": "R1", "pin_number": "2" }),
        )
        .await;
        assert_eq!(s["net"], "GND");
    }

    #[tokio::test]
    async fn the_rail_lists_the_components_on_it() {
        let s = call(SCH, "get_net_components", json!({ "net": "GND" })).await;
        let refs: Vec<&str> = s["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["reference"].as_str().unwrap())
            .collect();
        assert!(refs.contains(&"R1"), "R1 is on GND, got {refs:?}");
    }

    #[tokio::test]
    async fn tracing_a_point_on_the_rail_names_it() {
        let s = call(SCH, "trace_from_point", json!({ "x": 100.0, "y": 103.81 })).await;
        assert_eq!(s["net"], "GND");
    }

    #[tokio::test]
    async fn the_rail_is_listed_among_the_nets() {
        let s = call(SCH, "list_schematic_nets", json!({})).await;
        let nets: Vec<&str> = s["nets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(nets, vec!["GND", "SIG"]);
    }

    /// A rail carried onto a wire that also carries a named label is a short,
    /// and it was invisible while the graph knew only labels.
    #[tokio::test]
    async fn a_rail_shorted_to_a_named_net_is_reported() {
        let shorted = SCH.replace(
            "(label \"SIG\" (at 100 96.19 0)",
            "(label \"SIG\" (at 100 106 0)",
        );
        let s = call(&shorted, "find_shorted_nets", json!({})).await;
        assert_eq!(s["short_count"], 1, "SIG and GND share one wire: {s}");
    }
}

#[cfg(test)]
mod net_query_tests {
    use super::tool_call_support::{call, call_result};
    use super::*;
    use crate::mcp::protocol::ToolContent;

    const SCH: &str = include_str!("../../tests/fixtures/single_pin_nets.kicad_sch");
    const RAIL: &str = include_str!("../../tests/fixtures/power_rail.kicad_sch");

    #[tokio::test]
    async fn raw_and_escaped_slashes_resolve_the_same_saved_net() {
        // Real KiCad 10.0.8 emits one /A/B net with both pin members.
        let source = r#"(kicad_sch
          (lib_symbols (symbol "Test:P" (symbol "P_1_1"
            (pin passive line (at 0 0 0) (length 1) (name "~") (number "1")))))
          (label "A/B" (at 10 20 0) (uuid "plain"))
          (label "A{slash}B" (at 30 20 0) (uuid "escaped"))
          (symbol (lib_id "Test:P") (at 10 20 0) (unit 1) (uuid "first")
            (property "Reference" "R1"))
          (symbol (lib_id "Test:P") (at 30 20 0) (unit 1) (uuid "second")
            (property "Reference" "R2"))
        )"#;
        let listed = call(source, "list_schematic_nets", json!({})).await;
        assert_eq!(listed["nets"], json!(["A/B"]));
        for net in ["A/B", "A{slash}B"] {
            let members = call(source, "get_net_connections", json!({"net": net})).await;
            let pins = members["pins"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    (
                        p["reference"].as_str().unwrap(),
                        p["pin_number"].as_str().unwrap(),
                    )
                })
                .collect::<BTreeSet<_>>();
            assert_eq!(pins, BTreeSet::from([("R1", "1"), ("R2", "1")]));
            assert_eq!(members["label_count"], 2);
        }
        let diagnostics = call(source, "find_single_pin_nets", json!({})).await;
        assert_eq!(diagnostics["single_pin_net_count"], 0);
    }

    #[tokio::test]
    async fn a_nearby_same_net_wire_does_not_form_a_t_junction() {
        let near = r#"(kicad_sch
          (wire (pts (xy 10 20) (xy 30 20)) (uuid "horizontal"))
          (wire (pts (xy 15 10) (xy 15 19.995)) (uuid "vertical"))
          (label "SIG" (at 10 20 0))
          (label "SIG" (at 15 10 0))
        )"#;
        let result = call(near, "get_net_connectivity", json!({"net": "SIG"})).await;
        assert_eq!(result["wires"].as_array().unwrap().len(), 2);
        assert_eq!(result["t_junctions"], json!([]));

        let t = near.replace("19.995", "20");
        let result = call(&t, "get_net_connectivity", json!({"net": "SIG"})).await;
        assert_eq!(result["t_junctions"], json!([{"x": 15.0, "y": 20.0}]));
    }

    #[tokio::test]
    async fn named_net_reports_real_sheet_pins() {
        let ground = call(SCH, "get_net_connections", json!({ "net": "GND" })).await;
        let pins = ground["pins"].as_array().unwrap();
        let identities: BTreeSet<_> = pins
            .iter()
            .map(|pin| {
                (
                    pin["reference"].as_str().unwrap(),
                    pin["pin_number"].as_str().unwrap(),
                )
            })
            .collect();
        // KiCad 10.0.8's GND netlist also has R10.1 and R11.1 on the child sheet.
        assert_eq!(pins.len(), 7);
        assert_eq!(
            ground["labels"][0]["uuid"],
            "d6100939-dee2-4924-b9eb-80523f363191"
        );
        assert_eq!(
            identities,
            BTreeSet::from([
                ("R1", "1"),
                ("R3", "1"),
                ("R4", "1"),
                ("R6", "1"),
                ("R7", "1"),
                ("R8", "1"),
                ("R9", "1")
            ])
        );
        let split = call(SCH, "get_net_connections", json!({ "net": "SPLIT" })).await;
        assert_eq!(split["pins"].as_array().unwrap().len(), 2);
        let flagged = call(SCH, "get_net_connections", json!({ "net": "FLAGGED" })).await;
        assert_eq!(flagged["pins"].as_array().unwrap().len(), 1);

        let rail = call(RAIL, "get_net_connections", json!({ "net": "GND" })).await;
        assert_eq!(
            rail["pins"],
            json!([{
                "reference": "R1", "unit": 1, "pin_number": "2", "pin_name": "~",
                "electrical_type": "passive", "x": 100.0, "y": 103.81
            }])
        );
    }

    #[tokio::test]
    async fn midwire_pin_membership_requires_a_junction() {
        let crossed = RAIL
            .replace(
                "(label \"SIG\" (at 100 96.19 0)",
                "(label \"SIG\" (at 105 103.81 0)",
            )
            .replace(
                "(xy 100 103.81) (xy 100 110)",
                "(xy 95 103.81) (xy 105 103.81)",
            );
        let bare = call(&crossed, "get_net_connections", json!({ "net": "SIG" })).await;
        assert_eq!(bare["pins"], json!([]));

        let joined = crossed.replace(
            "(sheet_instances",
            "(junction (at 100 103.81) (diameter 0) (color 0 0 0 0) (uuid \"j1\")) (sheet_instances",
        );
        let joined = call(&joined, "get_net_connections", json!({ "net": "SIG" })).await;
        assert_eq!(joined["pins"].as_array().unwrap().len(), 1);
        assert_eq!(joined["pins"][0]["pin_number"], "2");
    }

    #[tokio::test]
    async fn net_queries_preserve_uuid_and_alias_membership() {
        let aliased = RAIL.replace(
            "(label \"SIG\" (at 100 96.19 0)",
            "(label \"ALT\" (at 100 110 0)",
        );
        let gnd = call(&aliased, "get_net_connections", json!({ "net": "GND" })).await;
        let alt = call(&aliased, "get_net_connections", json!({ "net": "ALT" })).await;
        assert_eq!(gnd["pins"], alt["pins"]);
        assert_eq!(gnd["labels"][0]["uuid"], "p1");
        assert_eq!(alt["labels"][0]["uuid"], "l1");
        assert_eq!(alt["label_count"], 1);

        let connectivity = call(&aliased, "get_net_connectivity", json!({ "net": "ALT" })).await;
        assert_eq!(connectivity["labels"], alt["labels"]);
        assert_eq!(connectivity["wires"][0]["uuid"], "w1");

        let legacy = aliased
            .replace(" (uuid \"l1\")", "")
            .replace(" (uuid \"w1\")", "");
        let connectivity = call(&legacy, "get_net_connectivity", json!({ "net": "ALT" })).await;
        assert_eq!(
            connectivity["labels"][0].get("uuid"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(
            connectivity["wires"][0].get("uuid"),
            Some(&serde_json::Value::Null)
        );
    }

    #[tokio::test]
    async fn net_queries_distinguish_absent_names_from_pinless_nets() {
        for tool in ["get_net_connections", "get_net_connectivity"] {
            for (name, suggestions) in [("NOPE", json!([])), ("gnd", json!(["GND"]))] {
                let result = call_result(SCH, tool, json!({ "net": name })).await;
                assert!(result.is_error, "{tool} accepted {name}");
                let ToolContent::Text { text } = &result.content[0] else {
                    panic!("expected text content");
                };
                let body: serde_json::Value = serde_json::from_str(text).unwrap();
                assert_eq!(
                    body["error"],
                    json!({
                        "kind": "not_found", "net": name, "suggestions": suggestions
                    })
                );
            }
            let stub = call(SCH, tool, json!({ "net": "STUB" })).await;
            assert_eq!(stub["labels"].as_array().unwrap().len(), 1);
            assert_eq!(
                stub["labels"][0]["uuid"],
                "485c0c1a-2a15-4c84-85da-d32ed20d613a"
            );

            let unwired = RAIL.replace(
                "(wire (pts (xy 100 103.81) (xy 100 110)) (uuid \"w1\"))",
                "",
            );
            let rail = call(&unwired, tool, json!({ "net": "GND" })).await;
            assert_eq!(rail["labels"][0]["uuid"], "p1");
            if tool == "get_net_connections" {
                assert_eq!(stub["pins"], json!([]));
                assert_eq!(rail["pins"], json!([]));
            } else {
                assert_eq!(rail["wires"], json!([]));
                assert_eq!(
                    stub["wires"][0]["uuid"],
                    "e20b570e-9f71-4204-a907-fdeb365956b8"
                );
            }
        }
    }
}

#[cfg(test)]
mod single_pin_net_tests {
    use super::tool_call_support::call;
    use super::*;

    /// A KiCad 10.0.5-authored parent sheet carrying one net of every shape the
    /// tool has to tell apart. Provenance and the KiCad netlist that fixes the
    /// expected pin count of each net are in `single_pin_nets.README.md`; the
    /// counts asserted below are that netlist's, not this crate's.
    ///
    /// - `TWO_PIN` — one label, R1.2 and R2.1. The ordinary way to draw a net,
    ///   and what the label count reported as a single-pin net.
    /// - `LONE` — two labels on two wire segments that never meet, one pin
    ///   between them. The defect the tool exists to find, invisible while a
    ///   second label made the count 2, and it can only be found by pooling the
    ///   two roots.
    /// - `SPLIT` — the same shape with a pin on each root. Pooling is what
    ///   keeps it off the report; KiCad's netlister agrees it is one 2-pin net.
    /// - `VCC` — a power symbol and one component pin.
    /// - `FLAGGED` — a `PWR_FLAG` (`#FLG01`) and one component pin.
    /// - `SHEET_NET` — R8.2 and a hierarchical sheet pin.
    /// - `MIXED` — a local and a global label naming one net that reaches one
    ///   pin.
    /// - `STUB` — a label on a wire that reaches no pin at all.
    /// - `GND` — the rail nine pins sit on, the control for a net that is
    ///   named by a power symbol and is not a defect.
    const SCH: &str = include_str!("../../tests/fixtures/single_pin_nets.kicad_sch");

    /// The child of that sheet: `SHEET_NET` continues here onto R10.2 through
    /// a hierarchical label, so each sheet on its own sees one pin of a net
    /// KiCad resolves to two.
    const CHILD: &str = include_str!("../../tests/fixtures/single_pin_nets_child.kicad_sch");

    async fn nets_of(sch: &str) -> Vec<serde_json::Value> {
        call(sch, "find_single_pin_nets", json!({})).await["nets"]
            .as_array()
            .unwrap()
            .clone()
    }

    async fn nets() -> Vec<serde_json::Value> {
        nets_of(SCH).await
    }

    fn net<'a>(nets: &'a [serde_json::Value], name: &str) -> Option<&'a serde_json::Value> {
        nets.iter().find(|net| net["net"] == name)
    }

    fn expect<'a>(nets: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
        net(nets, name).unwrap_or_else(|| panic!("{name} missing: {nets:?}"))
    }

    /// The false positive that made the tool unusable: 11 of 11 nets reported
    /// on a real sheet, every one of them wired to two or more pins.
    #[tokio::test]
    async fn a_net_with_one_label_and_two_pins_is_not_reported() {
        let nets = nets().await;

        assert!(net(&nets, "TWO_PIN").is_none(), "{nets:?}");
    }

    /// And the miss on the other side: a second label used to make a real
    /// single-pin net count 2 and disappear.
    #[tokio::test]
    async fn a_net_with_two_labels_and_one_pin_is_reported() {
        let nets = nets().await;
        let lone = expect(&nets, "LONE");

        assert_eq!(lone["pin_count"], 1);
        assert_eq!(lone["label_count"], 2, "the label count is kept, not used");
        assert_eq!(lone["pins"][0]["reference"], "R3");
        assert_eq!(lone["pins"][0]["pin"], "2");
    }

    /// `LONE`'s two labels sit on wire segments that never touch. KiCAD joins
    /// them by name, so the pins under both roots are one net's pins — count
    /// them per root and the one pin under the far root is a second net that
    /// reaches nothing.
    #[tokio::test]
    async fn labels_of_one_name_on_disconnected_roots_pool() {
        let nets = nets().await;

        assert_eq!(expect(&nets, "LONE")["pin_count"], 1);
    }

    /// The same two-root shape with a pin on each root. Pooling is the only
    /// reason this is not reported, and KiCad's own netlister resolves it to a
    /// single net of R4.2 and R5.1.
    #[tokio::test]
    async fn one_pin_under_each_of_two_pooled_roots_is_two_pins() {
        let nets = nets().await;

        assert!(net(&nets, "SPLIT").is_none(), "{nets:?}");
    }

    /// A rail that reaches exactly one component pin is the defect. The power
    /// symbol's own pin names it and must not make the count 2.
    #[tokio::test]
    async fn a_rail_reaching_one_pin_is_reported_without_its_power_symbol() {
        let nets = nets().await;
        let vcc = expect(&nets, "VCC");

        assert_eq!(vcc["pin_count"], 1);
        assert_eq!(vcc["pins"][0]["reference"], "R6", "not #PWR001");
    }

    /// `PWR_FLAG` is the other symbol whose pin names a net without consuming
    /// one, and it is not a `LabelKind::PowerSymbol` — its pin is `power_out`,
    /// so the `(power)` test lets it through. `#FLG01` is what keeps it out.
    #[tokio::test]
    async fn a_pwr_flag_pin_does_not_count_as_a_connection() {
        let nets = nets().await;
        let flagged = expect(&nets, "FLAGGED");

        assert_eq!(flagged["pin_count"], 1);
        assert_eq!(flagged["pins"][0]["reference"], "R7", "not #FLG01");
    }

    /// The rail nine pins sit on. A power symbol names it, so the tool has to
    /// count past the naming symbol without reporting the net.
    #[tokio::test]
    async fn a_rail_reaching_many_pins_is_not_reported() {
        let nets = nets().await;

        assert!(net(&nets, "GND").is_none(), "{nets:?}");
    }

    /// A net leaving the sheet is connected to whatever is on the other side.
    #[tokio::test]
    async fn a_hierarchical_sheet_pin_counts_as_a_pin() {
        let nets = nets().await;

        assert!(net(&nets, "SHEET_NET").is_none(), "{nets:?}");
    }

    /// The stub a deleted component leaves behind reaches nothing at all. The
    /// label count reported it, `find_orphan_items` names only its wire end,
    /// and counting pins must not drop it on the floor. KiCad's netlist has no
    /// `STUB` net to compare against, which is the point.
    #[tokio::test]
    async fn a_net_that_reaches_no_pin_is_reported_as_zero() {
        let nets = nets().await;
        let stub = expect(&nets, "STUB");

        assert_eq!(stub["pin_count"], 0);
        assert_eq!(stub["label_count"], 1);
        assert_eq!(stub["pins"].as_array().unwrap().len(), 0);
    }

    /// Local labels are extracted first, so `type` alone reports `NetLabel` for
    /// a net that also carries a global label and hides the one fact a caller
    /// needs: this count is not the whole net.
    #[tokio::test]
    async fn a_net_carrying_a_global_label_reports_every_kind_that_named_it() {
        let nets = nets().await;
        let mixed = expect(&nets, "MIXED");

        assert_eq!(mixed["type"], "NetLabel", "the compatibility field");
        assert_eq!(
            mixed["label_types"],
            json!(["GlobalLabel", "NetLabel"]),
            "sorted, and the global label is not lost"
        );
        assert_eq!(mixed["cross_sheet_unverified"], true);
    }

    /// A net named only by a local label is fully answered by this sheet.
    #[tokio::test]
    async fn a_locally_named_net_is_not_flagged_cross_sheet() {
        let nets = nets().await;
        let stub = expect(&nets, "STUB");

        assert_eq!(stub["label_types"], json!(["NetLabel"]));
        assert_eq!(stub["cross_sheet_unverified"], false);
    }

    /// A power symbol is a global label in KiCAD, so a rail counted on one
    /// sheet is a lead, not a finding — even when, as here, the project-wide
    /// netlist agrees that `VCC` reaches exactly one pin.
    #[tokio::test]
    async fn a_power_symbol_rail_is_flagged_cross_sheet() {
        let nets = nets().await;
        let vcc = expect(&nets, "VCC");

        assert_eq!(vcc["label_types"], json!(["PowerSymbol"]));
        assert_eq!(vcc["cross_sheet_unverified"], true);
    }

    /// The hierarchy boundary from the child's side. `SHEET_NET` reaches R10.2
    /// and its hierarchical label here, and R8.2 and the sheet pin on the
    /// parent; KiCad resolves the pair to one 2-pin net. Neither sheet can see
    /// that alone, so the child reports one pin and says it is unverified.
    #[tokio::test]
    async fn a_child_sheet_reports_a_hierarchical_net_as_unverified() {
        let nets = nets_of(CHILD).await;
        let sheet_net = expect(&nets, "SHEET_NET");

        assert_eq!(sheet_net["pin_count"], 1);
        assert_eq!(sheet_net["pins"][0]["reference"], "R10");
        assert_eq!(sheet_net["label_types"], json!(["HierarchicalLabel"]));
        assert_eq!(sheet_net["cross_sheet_unverified"], true);
    }

    /// And the control on the same child: a local net of two pins is answered
    /// there in full.
    #[tokio::test]
    async fn a_child_sheet_local_net_of_two_pins_is_not_reported() {
        let nets = nets_of(CHILD).await;

        assert!(net(&nets, "CHILD_LOCAL").is_none(), "{nets:?}");
    }

    /// The nets came out of a `HashMap` in whatever order it held them, so two
    /// runs of the same binary over the same sheet could disagree on the order.
    #[tokio::test]
    async fn the_report_is_sorted_and_stable() {
        let names: Vec<String> = nets()
            .await
            .iter()
            .map(|net| net["net"].as_str().unwrap().to_string())
            .collect();

        assert_eq!(names, vec!["FLAGGED", "LONE", "MIXED", "STUB", "VCC"]);
    }
}

#[cfg(test)]
mod multi_unit_tool_tests {
    use super::*;
    use crate::tools::ServerConfig;
    use std::io::Write;
    use std::sync::Arc;

    const ECC83: &str = include_str!("../../tests/fixtures/ecc83_multiunit.kicad_sch");

    async fn call_content(
        content: &str,
        tool: &str,
        mut args: serde_json::Value,
    ) -> serde_json::Value {
        let mut schematic = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        schematic.write_all(content.as_bytes()).unwrap();
        schematic.flush().unwrap();

        args["schematic"] = json!(schematic.path().to_str().unwrap());
        let definition = tools()
            .into_iter()
            .find(|tool_def| tool_def.name == tool)
            .unwrap();
        let context = ToolContext::new(
            ServerConfig::default(),
            Arc::new(crate::router::ToolRouter::new()),
        );
        let result = (definition.handler)(&args, Arc::new(context))
            .await
            .unwrap();
        assert!(!result.is_error, "{tool} failed");
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        serde_json::from_str(text).unwrap()
    }

    async fn call(tool: &str, args: serde_json::Value) -> serde_json::Value {
        call_content(ECC83, tool, args).await
    }

    #[tokio::test]
    async fn pin_lookup_uses_the_unit_that_owns_the_pin() {
        let result = call(
            "get_pin_connections",
            json!({ "reference": "U1", "pin_number": "4" }),
        )
        .await;

        let pin_x = result["pin_x"].as_f64().unwrap();
        let pin_y = result["pin_y"].as_f64().unwrap();
        assert!((pin_x - 60.96).abs() < 1e-9, "wrong unit x: {pin_x}");
        assert!((pin_y - 86.36).abs() < 1e-9, "wrong unit y: {pin_y}");
    }

    #[tokio::test]
    async fn component_nets_use_each_pins_own_unit_placement() {
        let result = call("get_component_nets", json!({ "reference": "U1" })).await;
        let pins = result["pins"].as_array().unwrap();
        assert_eq!(pins.len(), 9, "ECC83 must expose all three units: {pins:?}");

        let pin_four = pins
            .iter()
            .find(|pin| pin["pin"] == "4")
            .expect("heater pin 4");
        let pin_x = pin_four["x"].as_f64().unwrap();
        let pin_y = pin_four["y"].as_f64().unwrap();
        assert!((pin_x - 60.96).abs() < 1e-9, "wrong unit x: {pin_x}");
        assert!((pin_y - 86.36).abs() < 1e-9, "wrong unit y: {pin_y}");
    }

    #[tokio::test]
    async fn net_components_ignore_pins_from_unplaced_units() {
        // Applying the heater unit's pin 5 geometry to unit 2's placement
        // invents a pin at (160.02, 119.38); no placed ECC83 unit owns it.
        let phantom_label =
            "\t(label \"PHANTOM_TEST\" (at 160.02 119.38 0) (uuid \"00000000-0000-4000-8000-000000000183\"))\n";
        let content = ECC83.replacen(
            "\t(sheet_instances",
            &format!("{phantom_label}\t(sheet_instances"),
            1,
        );
        assert_ne!(content, ECC83, "fixture insertion point changed");

        let result = call_content(
            &content,
            "get_net_components",
            json!({ "net": "PHANTOM_TEST" }),
        )
        .await;
        assert!(
            result["components"]
                .as_array()
                .unwrap()
                .iter()
                .all(|component| component["reference"] != "U1"),
            "unit 2 must not invent heater pin 5: {result}"
        );
    }

    #[tokio::test]
    async fn connected_items_include_nets_from_every_placed_unit() {
        let heater_label =
            "\t(label \"HEATER_TEST\" (at 66.04 86.36 0) (uuid \"00000000-0000-4000-8000-000000000184\"))\n";
        let content = ECC83.replacen(
            "\t(sheet_instances",
            &format!("{heater_label}\t(sheet_instances"),
            1,
        );
        assert_ne!(content, ECC83, "fixture insertion point changed");

        let result = call_content(
            &content,
            "get_connected_items",
            json!({ "reference": "U1" }),
        )
        .await;
        assert!(
            result["nets"]
                .as_array()
                .unwrap()
                .iter()
                .any(|net| net == "HEATER_TEST"),
            "heater unit net missing: {result}"
        );
    }
}

/// The read-only side of net identity, on the real KiCad fixture.
///
/// Provenance, KiCad's own netlist and the ERC precedence statements are in
/// `two_name_nets.README.md`; every expectation below is that table.
#[cfg(test)]
mod two_name_net_tests {
    use super::tool_call_support::call;
    use super::*;

    const SCH: &str = include_str!("../../tests/fixtures/two_name_nets.kicad_sch");

    /// Every multi-name net on this sheet answers with the name KiCad prints,
    /// so the five `multiple_net_names` outcomes in the ERC report are all
    /// asserted here. Twenty runs: the old scan returned the first `point_nets`
    /// entry on the net's root, so the answer moved between runs of the same
    /// binary rather than within one.
    #[tokio::test]
    async fn a_multi_name_net_answers_with_kicads_name() {
        // pin, and the name KiCad's netlist gives the net it is on.
        for (reference, pin, expected) in [
            // The five nets carrying more than one name, one per precedence rule.
            ("U1", "8", "+3V3"),   // power symbol over the VCC and ALT labels
            ("U1", "4", "RETURN"), // global label over the GND symbols
            ("R1", "1", "SYS"),    // global over both the +5V symbol and PULLUP
            ("TP2", "1", "AAA"),   // two locals, the lexically first one
            ("TP4", "1", "MIX"),   // local label over the hierarchical one
            // And a single-name net whose two segments are joined by that name.
            ("R1", "2", "SDA"),
        ] {
            for attempt in 0..20 {
                let body = call(
                    SCH,
                    "get_pin_connections",
                    json!({ "reference": reference, "pin_number": pin }),
                )
                .await;
                assert_eq!(
                    body["net"], expected,
                    "{reference}.{pin} on attempt {attempt}: {body}"
                );
            }
        }
    }

    /// `get_connected_items` answers about the component's *nets*: everything
    /// on them, including the parts reachable only by name and the aliases the
    /// net is not called by.
    #[tokio::test]
    async fn connected_items_span_every_segment_and_alias_of_the_net() {
        let items = call(SCH, "get_connected_items", json!({ "reference": "U1" })).await;

        let mut nets: Vec<&str> = items["nets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|net| net.as_str().unwrap())
            .collect();
        nets.sort_unstable();
        assert_eq!(nets, ["+3V3", "RETURN", "SCL", "SDA"], "{items}");

        let components: Vec<&str> = items["connected_components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|component| component["reference"].as_str().unwrap())
            .collect();
        for reachable_by_name_only in ["C1", "C2", "TP1", "TP7", "R1", "R2", "C3"] {
            assert!(
                components.contains(&reachable_by_name_only),
                "{reachable_by_name_only} is on one of U1's nets: {items}"
            );
        }

        let labels: Vec<&str> = items["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|label| label["net"].as_str().unwrap())
            .collect();
        // The names these nets are *not* called by are still on them: `VCC` and
        // `ALT` on the rail KiCad calls `+3V3`, `GND` on the one it calls
        // `RETURN`. (`PULLUP` names the pull-up rail, which U1 is not on — its
        // SDA/SCL pins reach the resistors, not the rail behind them.)
        for alias in ["VCC", "ALT", "GND"] {
            assert!(labels.contains(&alias), "alias {alias} dropped: {items}");
        }
    }

    /// A net with no label at all is still a net. Collecting by name left it
    /// out of the point set entirely, so the wire and the part at its other end
    /// went unreported.
    #[tokio::test]
    async fn an_unnamed_net_reports_its_wire_and_the_part_at_the_far_end() {
        let items = call(SCH, "get_connected_items", json!({ "reference": "TP8" })).await;

        assert_eq!(items["connected_wires"], 1, "{items}");
        let components: Vec<&str> = items["connected_components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|component| component["reference"].as_str().unwrap())
            .collect();
        assert_eq!(components, ["TP9"], "{items}");
    }
}

/// What `trace_from_point` reports as being at a point, on the real KiCad
/// fixture (#539). Wires and labels were already answered; a pin and a junction
/// dot were not, and there was no empty key to show they had been skipped.
#[cfg(test)]
mod trace_from_point_tests {
    use super::tool_call_support::{call, call_result};
    use super::*;

    const SCH: &str = include_str!("../../tests/fixtures/trace_point_kicad10.kicad_sch");

    async fn at(x: f64, y: f64) -> serde_json::Value {
        call(SCH, "trace_from_point", json!({ "x": x, "y": y })).await
    }

    async fn at_within(x: f64, y: f64, tolerance: serde_json::Value) -> serde_json::Value {
        call(
            SCH,
            "trace_from_point",
            json!({ "x": x, "y": y, "tolerance": tolerance }),
        )
        .await
    }

    /// The references and pin numbers reported at a point, sorted, so the
    /// assertion does not depend on the order symbols sit in the file.
    fn pins(trace: &serde_json::Value) -> Vec<String> {
        let mut found: Vec<String> = trace["pins_here"]
            .as_array()
            .expect("pins_here is always present")
            .iter()
            .map(|pin| {
                format!(
                    "{}.{}",
                    pin["reference"].as_str().unwrap(),
                    pin["pin"].as_str().unwrap()
                )
            })
            .collect();
        found.sort();
        found
    }

    /// How many entries a `*_here` key carries. Every one of them is always
    /// present, so an absent key is a failure rather than a zero.
    fn count(trace: &serde_json::Value, key: &str) -> usize {
        trace[key]
            .as_array()
            .unwrap_or_else(|| panic!("{key} is always present: {trace}"))
            .len()
    }

    /// The filed case: `R1` pin 1, two wire ends, a junction dot and net `NETA`
    /// all at `(100.33, 96.52)`. The net and the wires were already right; the
    /// pin and the dot were the two the response dropped.
    #[tokio::test]
    async fn the_pin_and_the_dot_at_a_point_are_both_reported() {
        let trace = at(100.33, 96.52).await;

        assert_eq!(trace["net"], "NETA", "{trace}");
        assert_eq!(count(&trace, "wires_here"), 2, "{trace}");
        assert_eq!(pins(&trace), ["R1.1"], "{trace}");
        assert_eq!(count(&trace, "junctions_here"), 1, "{trace}");
    }

    /// A reported pin carries enough to act on without a second call.
    #[tokio::test]
    async fn a_reported_pin_names_itself_and_its_position() {
        let trace = at(100.33, 96.52).await;
        let pin = &trace["pins_here"][0];

        assert_eq!(pin["reference"], "R1", "{trace}");
        assert_eq!(pin["pin"], "1", "{trace}");
        assert_eq!(pin["electrical_type"], "passive", "{trace}");
        assert_eq!(pin["x"], 100.33, "{trace}");
        assert_eq!(pin["y"], 96.52, "{trace}");
    }

    /// `R1` pin 2, wired but with no dot on it. A pin must not imply a junction.
    #[tokio::test]
    async fn a_pin_without_a_dot_reports_no_junction() {
        let trace = at(100.33, 104.14).await;

        assert_eq!(pins(&trace), ["R1.2"], "{trace}");
        assert_eq!(count(&trace, "junctions_here"), 0, "{trace}");
        assert_eq!(count(&trace, "wires_here"), 1, "{trace}");
    }

    /// A branch landing mid-span: the dot `add_wire` inserts, with no pin under
    /// it. A junction must not imply a pin either.
    #[tokio::test]
    async fn a_dot_without_a_pin_reports_no_pin() {
        let trace = at(130.81, 121.92).await;

        assert_eq!(count(&trace, "junctions_here"), 1, "{trace}");
        assert!(pins(&trace).is_empty(), "{trace}");
        assert_eq!(count(&trace, "wires_here"), 2, "{trace}");
    }

    /// `R2` pin 2 and `R3` pin 1 sit on one point with no wire between them —
    /// KiCad's own netlist resolves them as the single net `Net-(R2-Pad2)`.
    /// Reporting one of them would be the same defect in a smaller form.
    #[tokio::test]
    async fn both_of_two_stacked_pins_are_reported() {
        let trace = at(160.02, 104.14).await;

        assert_eq!(pins(&trace), ["R2.2", "R3.1"], "{trace}");
    }

    /// Both units of `U1` are placed, 30mm apart. Reading the pins off the
    /// library symbol rather than the placed unit would put unit 1's pins on
    /// unit 2's point (#35).
    #[tokio::test]
    async fn a_point_on_one_unit_reports_only_that_unit() {
        let unit_two = at(198.12, 130.81).await;
        assert_eq!(pins(&unit_two), ["U1.7"], "{unit_two}");

        let unit_one = at(198.12, 100.33).await;
        assert_eq!(pins(&unit_one), ["U1.1"], "{unit_one}");
    }

    /// The omission was invisible because no key was there to be empty. On bare
    /// paper all four lists are present and empty.
    #[tokio::test]
    async fn a_bare_point_answers_with_four_empty_lists() {
        let trace = at(210.0, 50.0).await;

        assert_eq!(trace["net"], serde_json::Value::Null, "{trace}");
        for key in ["wires_here", "labels_here", "pins_here", "junctions_here"] {
            assert_eq!(count(&trace, key), 0, "{key} should be empty: {trace}");
        }
    }

    /// A tolerance no coincidence test can pass used to be accepted, and the
    /// response then asserted `net: "NETA"` at a point it reported as bare:
    /// the net comes from the shared graph, which resolves at its own fixed
    /// tolerance and never saw the argument. Empty evidence and a named net in
    /// one successful answer is the contradiction; refusing the argument is the
    /// fix. `0` is refused too — a point has no zero-width neighbourhood.
    #[tokio::test]
    async fn an_unusable_tolerance_is_refused_not_answered_empty() {
        for tolerance in [json!(0.0), json!(-1.0), json!("wide")] {
            let result = call_result(
                SCH,
                "trace_from_point",
                json!({ "x": 100.33, "y": 96.52, "tolerance": tolerance }),
            )
            .await;

            assert!(result.is_error, "accepted tolerance {tolerance}");
            let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
                panic!("expected text content");
            };
            let body: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
            assert_eq!(body["error"]["field"], "tolerance", "{body}");
            assert!(body.get("net").is_none(), "a refusal names no net: {body}");
        }
    }

    /// The refusal is of unusable values, not of the argument: a positive
    /// tolerance still answers, and the filed point answers the same.
    #[tokio::test]
    async fn a_positive_tolerance_is_still_accepted() {
        let trace = at_within(100.33, 96.52, json!(0.05)).await;

        assert_eq!(trace["net"], "NETA", "{trace}");
        assert_eq!(pins(&trace), ["R1.1"], "{trace}");
        assert_eq!(count(&trace, "junctions_here"), 1, "{trace}");
    }

    /// Pins and dots are read from `ConnectivityIndex` at the tolerance the
    /// caller asked for, so the same probe 0.02 mm off `R1` pin 1 finds it at
    /// `0.05` and does not at `0.01`. A scan that hardcoded a tolerance, or an
    /// index built at one while the probe used another, fails one of these.
    #[tokio::test]
    async fn the_requested_tolerance_governs_the_pins_and_dots_reported() {
        let wide = at_within(100.35, 96.52, json!(0.05)).await;
        assert_eq!(pins(&wide), ["R1.1"], "{wide}");
        assert_eq!(count(&wide, "junctions_here"), 1, "{wide}");

        let tight = at_within(100.35, 96.52, json!(0.01)).await;
        assert!(pins(&tight).is_empty(), "{tight}");
        assert_eq!(count(&tight, "junctions_here"), 0, "{tight}");
    }
}
