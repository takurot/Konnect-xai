//! `sch_bus` toolset — buses, bus entries, and pin-to-bus fan-out.
//!
//! A KiCad bus is three things drawn together, and all three have to agree or
//! the bus carries nothing:
//!
//!   1. the **bus segment** itself — geometrically a wire, distinguished only
//!      by the node name and by the label attached to it;
//!   2. a **bus entry** per member — the 45° tick that bridges the gap between
//!      a wire and the bus. It is not decoration: without it the wire and the
//!      bus do not touch, because they are deliberately drawn apart;
//!   3. a **label on every wire stub**, naming the member signal, plus a label
//!      on the bus naming the group.
//!
//! Membership is by *name*, never by geometry: a stub joins the bus because its
//! label matches a member of the bus label, so `connect_pins_to_bus` writes all
//! three parts from one call rather than leaving the caller to line them up.
//!
//! Bus label syntax, both accepted by `add_schematic_net_label`:
//!   - vector  `X_DIG[1..6]`   — members `X_DIG1` … `X_DIG6`
//!   - group   `{A B C}`       — members named individually

use super::sch_route_safety::{allow_join, check_route};
use super::sch_wiring::{ambiguous_reference_result, resolve_pin_endpoint};
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::{get_path, opt_f64, opt_str, require_f64, ToolContext, ToolDef};
use konnect_sexp::{
    parser::parse_sexp,
    schematic::{
        extract_symbol_instances, format_bus, format_bus_entry, format_net_label, format_wire,
        BusEntryDirection,
    },
    writer::write_atomic_if_unchanged,
};
use serde_json::json;

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "add_bus",
            "Add a bus segment between two points. Geometrically a wire, but KiCad treats it as a \
             bus: it carries the members named by the bus label attached to it (vector \
             'NAME[1..6]' or group '{A B C}'), and ordinary wires join it only through a bus entry.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "x1": { "type": "number" },
                    "y1": { "type": "number" },
                    "x2": { "type": "number" },
                    "y2": { "type": "number" }
                },
                "required": ["schematic", "x1", "y1", "x2", "y2"]
            }),
            |args, ctx| async move { handle_add_bus(args, ctx).await }
        ),
        tool!(
            "batch_add_bus",
            "Add multiple bus segments in a single file read/write cycle.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "buses": {
                        "type": "array",
                        "description": "List of {x1,y1,x2,y2} segments",
                        "items": {
                            "type": "object",
                            "properties": {
                                "x1": { "type": "number" }, "y1": { "type": "number" },
                                "x2": { "type": "number" }, "y2": { "type": "number" }
                            },
                            "required": ["x1", "y1", "x2", "y2"]
                        }
                    }
                },
                "required": ["schematic", "buses"]
            }),
            |args, ctx| async move { handle_batch_add_bus(args, ctx).await }
        ),
        tool!(
            "add_bus_entry",
            "Add a bus entry — the 45-degree tick that connects a wire to a bus. Required: a wire \
             and a bus that merely touch are NOT connected without one. 'x'/'y' are the end on \
             the wire side; 'direction' says which way the tick runs to reach the bus.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "x": { "type": "number", "description": "X of the wire-side end" },
                    "y": { "type": "number", "description": "Y of the wire-side end" },
                    "direction": {
                        "type": "string",
                        "description": "Which way the tick runs from 'at' to the bus",
                        "enum": ["down_right", "down_left", "up_right", "up_left"],
                        "default": "down_right"
                    }
                },
                "required": ["schematic", "x", "y"]
            }),
            |args, ctx| async move { handle_add_bus_entry(args, ctx).await }
        ),
        tool!(
            "connect_pins_to_bus",
            "Fan a set of pins out onto a bus: for each pin, a wire stub from the pin to the bus \
             entry, the bus entry itself, and a net label naming the member. This is the whole \
             connection — a stub without a label joins nothing, since bus membership is by name. \
             Give the bus's fixed coordinate ('bus_y' for a horizontal bus, 'bus_x' for a \
             vertical one); the bus segment itself is drawn separately with add_bus.",
            json!({
                "type": "object",
                "properties": {
                    "allow_join": {"type":"boolean","default":false,"description":"Explicitly permit stub routes to touch additional nets; otherwise the complete batch is refused."},
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "bus_y": { "type": "number", "description": "Y of a horizontal bus (omit if vertical)" },
                    "bus_x": { "type": "number", "description": "X of a vertical bus (omit if horizontal)" },
                    "connections": {
                        "type": "array",
                        "description": "List of {reference, pin_number, net} — net is the bus member name",
                        "items": {
                            "type": "object",
                            "properties": {
                                "reference": { "type": "string" },
                                "pin_number": { "type": "string" },
                                "net": { "type": "string" }
                            },
                            "required": ["reference", "pin_number", "net"]
                        }
                    },
                },
                "required": ["schematic", "connections"]
            }),
            |args, ctx| async move { handle_connect_pins_to_bus(args, ctx).await }
        ),
    ]
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn handle_add_bus(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let (x1, y1, x2, y2) = match read_xy_pair(args) {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    if (x1 - x2).abs() > 0.01 && (y1 - y2).abs() > 0.01 {
        return Ok(CallToolResult::error(
            "Bus segment must be horizontal or vertical",
        ));
    }

    let content = std::fs::read_to_string(&sch_path)?;
    let new_content =
        crate::tools::sch_wiring::insert_before_close(&content, &format_bus(x1, y1, x2, y2));
    write_atomic_if_unchanged(&sch_path, &content, &new_content)?;

    Ok(CallToolResult::json(&json!({
        "added": "bus",
        "from": { "x": x1, "y": y1 },
        "to": { "x": x2, "y": y2 }
    })))
}

async fn handle_batch_add_bus(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let items = match args["buses"].as_array() {
        Some(a) => a.clone(),
        None => return Ok(CallToolResult::error("Missing 'buses' array")),
    };

    let mut inserts = String::new();
    let mut added: Vec<serde_json::Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for (i, item) in items.iter().enumerate() {
        let (x1, y1, x2, y2) = match read_xy_pair(item) {
            Ok(v) => v,
            Err(_) => {
                errors.push(format!("Segment {i}: needs x1, y1, x2, y2"));
                continue;
            }
        };
        if (x1 - x2).abs() > 0.01 && (y1 - y2).abs() > 0.01 {
            errors.push(format!("Segment {i}: must be horizontal or vertical"));
            continue;
        }
        inserts.push('\n');
        inserts.push_str("  ");
        inserts.push_str(&format_bus(x1, y1, x2, y2));
        added.push(json!({ "from": {"x": x1, "y": y1}, "to": {"x": x2, "y": y2} }));
    }

    let content = std::fs::read_to_string(&sch_path)?;
    let new_content = crate::tools::sch_wiring::insert_before_close(&content, &inserts);
    write_atomic_if_unchanged(&sch_path, &content, &new_content)?;

    Ok(CallToolResult::json(&json!({
        "added_count": added.len(),
        "added": added,
        "errors": errors
    })))
}

async fn handle_add_bus_entry(
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
    let (x, y) = konnect_sexp::geometry::snap_point(x, y, 1.27);
    let raw = opt_str(args, "direction").unwrap_or("down_right");
    let direction = match parse_direction(raw) {
        Some(d) => d,
        None => {
            return Ok(CallToolResult::error(format!(
                "Unknown direction '{raw}'. Valid: down_right, down_left, up_right, up_left"
            )))
        }
    };

    let content = std::fs::read_to_string(&sch_path)?;
    let new_content =
        crate::tools::sch_wiring::insert_before_close(&content, &format_bus_entry(x, y, direction));
    write_atomic_if_unchanged(&sch_path, &content, &new_content)?;

    // Which end is the bus side is a fact about the sheet, not about the
    // request: the entry's `at` corner is wherever the caller put it. The
    // documented convention (x/y = wire side) only holds when the caller
    // follows it, and the response used to assert it unchecked — labelling
    // the on-bus corner `wire_side` for anyone who gave the bus point
    // instead (#329). Judge both corners against the sheet's actual bus
    // segments and only fall back to the convention when geometry cannot
    // decide.
    let far = (
        konnect_sexp::geometry::round6(x + direction.size().0),
        konnect_sexp::geometry::round6(y + direction.size().1),
    );
    let (at_on_bus, far_on_bus) = match parse_sexp(&content) {
        Ok(tree) => {
            let buses = konnect_sexp::schematic::extract_buses(&tree);
            let on_bus = |p: (f64, f64)| {
                buses.iter().any(|bus| {
                    konnect_sexp::geometry::point_on_segment(
                        p.0,
                        p.1,
                        bus.x1,
                        bus.y1,
                        bus.x2,
                        bus.y2,
                        crate::tools::sch_connectivity::COINCIDENT_TOLERANCE,
                    )
                })
            };
            (on_bus((x, y)), on_bus(far))
        }
        Err(_) => (false, false),
    };
    let (bus_side, wire_side, note) = match (at_on_bus, far_on_bus) {
        (true, false) => ((x, y), far, None),
        (false, true) => (far, (x, y), None),
        (true, true) => (
            far,
            (x, y),
            Some("both ends of this entry touch a bus; sides are reported from the documented convention (x/y = wire side)"),
        ),
        (false, false) => (
            far,
            (x, y),
            Some("no bus touches this entry; sides are reported from the documented convention (x/y = wire side)"),
        ),
    };
    let mut response = json!({
        "added": "bus_entry",
        "wire_side": { "x": wire_side.0, "y": wire_side.1 },
        "bus_side": { "x": bus_side.0, "y": bus_side.1 }
    });
    if let Some(note) = note {
        response["note"] = json!(note);
    }
    Ok(CallToolResult::json(&response))
}

async fn handle_connect_pins_to_bus(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let allow = match allow_join(args) {
        Ok(value) => value,
        Err(error) => return Ok(error),
    };
    let conns = match args["connections"].as_array() {
        Some(a) => a.clone(),
        None => return Ok(CallToolResult::error("Missing 'connections' array")),
    };
    let bus_y = opt_f64(args, "bus_y").map(snap_coordinate);
    let bus_x = opt_f64(args, "bus_x").map(snap_coordinate);
    let size = 2.54; // BusEntryDirection fixes the tick at one grid step

    let horizontal = match (bus_y, bus_x) {
        (Some(_), None) => true,
        (None, Some(_)) => false,
        _ => {
            return Ok(CallToolResult::error(
                "Give exactly one of 'bus_y' (horizontal bus) or 'bus_x' (vertical bus)",
            ))
        }
    };

    let content = std::fs::read_to_string(&sch_path)?;
    let tree = match parse_sexp(&content) {
        Ok(t) => t,
        Err(e) => return Ok(CallToolResult::error(format!("Parse error: {e}"))),
    };
    let instances = extract_symbol_instances(&tree);
    let lib_syms = tree
        .find("lib_symbols")
        .map(|n| n.find_all("symbol"))
        .unwrap_or_default();

    let mut inserts = String::new();
    let mut added: Vec<serde_json::Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for conn in &conns {
        let (reference, pin_number, net) = match (
            conn["reference"].as_str(),
            conn["pin_number"].as_str(),
            conn["net"].as_str(),
        ) {
            (Some(r), Some(p), Some(n)) => (r, p, n),
            _ => {
                errors.push("Each connection needs reference, pin_number and net".into());
                continue;
            }
        };

        let (px, py) = match resolve_pin_endpoint(&instances, &lib_syms, reference, pin_number) {
            Ok(point) => point,
            Err(error) => {
                if let Some(refusal) = ambiguous_reference_result(&error) {
                    return Ok(refusal);
                }
                errors.push(error.to_string());
                continue;
            }
        };

        // Pin coordinates are observed geometry, not caller placement requests.
        // Half-grid pins (0.635 mm) must stay at their real endpoints. Only
        // caller-supplied bus geometry is snapped to the 1.27 mm default grid.
        // The entry bridges the last `size` before the bus; the stub covers the
        // rest of the way from the pin.
        let (entry_x, entry_y, dx, dy, sx, sy) = if horizontal {
            let by = bus_y.unwrap();
            let dir = if by > py { 1.0 } else { -1.0 };
            let ey = konnect_sexp::geometry::round6(by - dir * size);
            (px, ey, dir * size, dir * size, px, py)
        } else {
            let bx = bus_x.unwrap();
            let dir = if bx > px { 1.0 } else { -1.0 };
            let ex = konnect_sexp::geometry::round6(bx - dir * size);
            (ex, py, dir * size, dir * size, px, py)
        };

        let staged_source = crate::tools::sch_wiring::insert_before_close(&content, &inserts);
        let staged_tree = parse_sexp(&staged_source)?;
        if let Err(refusal) = check_route(
            &staged_tree,
            &[((sx, sy), (entry_x, entry_y))],
            &[(px, py)],
            Some(net),
            allow,
        ) {
            return Ok(refusal);
        }

        // Stub from the pin up to the entry's wire-side end. Zero-length when
        // the pin already sits there; KiCad rejects a degenerate wire.
        if (sx - entry_x).abs() > 0.01 || (sy - entry_y).abs() > 0.01 {
            inserts.push_str("\n  ");
            inserts.push_str(&format_wire(sx, sy, entry_x, entry_y));
        }
        inserts.push_str("\n  ");
        inserts.push_str(&format_bus_entry(entry_x, entry_y, entry_dir(dx, dy)));
        // The label is what actually puts this stub on the bus.
        let rot = if horizontal { 90.0 } else { 0.0 };
        inserts.push_str("\n  ");
        inserts.push_str(&format_net_label(net, sx, sy, rot));

        added.push(json!({
            "reference": reference, "pin": pin_number, "net": net,
            "pin_at": { "x": px, "y": py },
            "entry_at": { "x": entry_x, "y": entry_y }
        }));
    }

    let new_content = crate::tools::sch_wiring::insert_before_close(&content, &inserts);
    write_atomic_if_unchanged(&sch_path, &content, &new_content)?;

    Ok(CallToolResult::json(&json!({
        "connected_count": added.len(),
        "connected": added,
        "errors": errors
    })))
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Map a signed (dx, dy) tick offset onto KiCad's four bus-entry directions.
fn entry_dir(dx: f64, dy: f64) -> BusEntryDirection {
    match (dx > 0.0, dy > 0.0) {
        (true, true) => BusEntryDirection::DownRight,
        (false, true) => BusEntryDirection::DownLeft,
        (true, false) => BusEntryDirection::UpRight,
        (false, false) => BusEntryDirection::UpLeft,
    }
}

fn parse_direction(s: &str) -> Option<BusEntryDirection> {
    Some(match s {
        "down_right" => BusEntryDirection::DownRight,
        "down_left" => BusEntryDirection::DownLeft,
        "up_right" => BusEntryDirection::UpRight,
        "up_left" => BusEntryDirection::UpLeft,
        _ => return None,
    })
}

fn snap_coordinate(value: f64) -> f64 {
    konnect_sexp::geometry::snap_to_grid(value, 1.27)
}

fn read_xy_pair(v: &serde_json::Value) -> Result<(f64, f64, f64, f64), CallToolResult> {
    Ok((
        snap_coordinate(require_f64(v, "x1")?),
        snap_coordinate(require_f64(v, "y1")?),
        snap_coordinate(require_f64(v, "x2")?),
        snap_coordinate(require_f64(v, "y2")?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet_with_bus() -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bus.kicad_sch");
        let mut content = String::from(
            "(kicad_sch\n  (version 20260306)\n  (uuid \"11111111-1111-4111-8111-111111111111\")\n",
        );
        content.push_str(&format_bus(100.33, 100.33, 150.11, 100.33));
        content.push_str("\n)\n");
        std::fs::write(&path, content).unwrap();
        (directory, path)
    }

    async fn entry_response(path: &std::path::Path, x: f64, y: f64) -> serde_json::Value {
        let result = handle_add_bus_entry(
            &json!({ "schematic": path, "x": x, "y": y, "direction": "down_right" }),
            &crate::tools::ToolContext::new(
                crate::tools::ServerConfig::default(),
                std::sync::Arc::new(crate::router::ToolRouter::new()),
            ),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "add_bus_entry failed");
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text");
        };
        serde_json::from_str(text).unwrap()
    }

    /// #329: which end touches the bus is a fact about the sheet. The caller
    /// here places the entry's `at` ON the bus (contrary to the documented
    /// x/y-is-the-wire-side convention, but exactly how people use it), and
    /// the response must label the corners from the geometry, not the
    /// convention — the old response called the on-bus corner `wire_side`,
    /// and a caller following it attached the wire to the bus itself, where
    /// it connects nothing.
    #[tokio::test]
    async fn bus_side_is_the_corner_that_actually_touches_the_bus() {
        let (_directory, path) = sheet_with_bus();
        let response = entry_response(&path, 120.65, 100.33).await;
        assert_eq!(response["bus_side"]["x"], 120.65, "{response}");
        assert_eq!(response["bus_side"]["y"], 100.33);
        assert_eq!(response["wire_side"]["x"], 123.19);
        assert_eq!(response["wire_side"]["y"], 102.87);
        assert!(
            response.get("note").is_none(),
            "geometry decided: {response}"
        );
    }

    /// The documented usage — x/y at the wire end, the tick running to the
    /// bus — keeps its labels, now confirmed by geometry rather than assumed.
    #[tokio::test]
    async fn the_documented_wire_side_placement_keeps_its_labels() {
        let (_directory, path) = sheet_with_bus();
        let response = entry_response(&path, 120.65, 97.79).await;
        assert_eq!(response["wire_side"]["x"], 120.65, "{response}");
        assert_eq!(response["wire_side"]["y"], 97.79);
        assert_eq!(response["bus_side"]["x"], 123.19);
        assert_eq!(response["bus_side"]["y"], 100.33);
        assert!(response.get("note").is_none(), "{response}");
    }

    /// With no bus near the entry, geometry cannot decide — the response
    /// falls back to the documented convention and says so instead of
    /// silently asserting.
    #[tokio::test]
    async fn a_floating_entry_reports_the_convention_and_says_so() {
        let (_directory, path) = sheet_with_bus();
        let response = entry_response(&path, 30.0, 30.0).await;
        assert_eq!(response["wire_side"]["x"], 30.48, "{response}");
        assert_eq!(response["bus_side"]["x"], 33.02);
        let note = response["note"].as_str().expect("note present");
        assert!(note.contains("no bus touches"), "{note}");
    }

    #[test]
    fn bus_sexp_is_a_bus_node_not_a_wire() {
        let s = format_bus(10.0, 20.0, 50.0, 20.0);
        assert!(s.starts_with("(bus"), "got {s}");
        assert!(s.contains("(xy 10 20) (xy 50 20)"));
    }

    /// `size` is the offset to the bus side, so the two ends are `at` and
    /// `at + size`. Getting the sign wrong puts the tick on the far side of the
    /// bus, where it connects nothing.
    #[test]
    fn entry_direction_follows_the_offset_signs() {
        assert!(matches!(
            entry_dir(2.54, 2.54),
            BusEntryDirection::DownRight
        ));
        assert!(matches!(
            entry_dir(-2.54, 2.54),
            BusEntryDirection::DownLeft
        ));
        assert!(matches!(entry_dir(2.54, -2.54), BusEntryDirection::UpRight));
        assert!(matches!(entry_dir(-2.54, -2.54), BusEntryDirection::UpLeft));
    }

    #[test]
    fn direction_names_round_trip() {
        for (name, dir) in [
            ("down_right", BusEntryDirection::DownRight),
            ("down_left", BusEntryDirection::DownLeft),
            ("up_right", BusEntryDirection::UpRight),
            ("up_left", BusEntryDirection::UpLeft),
        ] {
            assert_eq!(parse_direction(name).unwrap().size(), dir.size());
        }
        assert!(parse_direction("sideways").is_none());
    }

    #[test]
    fn all_four_tools_are_registered() {
        let names: Vec<_> = tools().iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            vec![
                "add_bus",
                "batch_add_bus",
                "add_bus_entry",
                "connect_pins_to_bus"
            ]
        );
    }
}

#[cfg(test)]
mod file_safety_tests {
    use super::*;
    #[tokio::test]
    async fn bus_coordinates_snap_before_persisting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grid.kicad_sch");
        std::fs::write(&path, "(kicad_sch)").unwrap();
        let ctx = ToolContext::new(
            Default::default(),
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        );
        let result = handle_add_bus(
            &json!({"schematic":path,"x1":50.8001,"y1":25.4001,"x2":76.2001,"y2":25.4001}),
            &ctx,
        )
        .await
        .unwrap();
        assert!(!result.is_error);
        let text = std::fs::read_to_string(path).unwrap();
        let root = parse_sexp(&text).unwrap();
        let bus = konnect_sexp::schematic::extract_buses(&root).remove(0);
        assert_eq!((bus.x1, bus.y1, bus.x2, bus.y2), (50.8, 25.4, 76.2, 25.4));
    }
}

#[cfg(test)]
mod fine_grid_pin_tests {
    use super::*;

    #[tokio::test]
    async fn bus_fanout_preserves_the_actual_half_grid_pin_endpoint() {
        for horizontal in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("fine-grid.kicad_sch");
            std::fs::write(
                &path,
                r#"(kicad_sch
              (lib_symbols (symbol "Test:P" (symbol "P_1_1"
                (pin passive line (at 0 0 0) (length 0) (name "P") (number "1")))))
              (symbol (lib_id "Test:P") (at 50.165 50.165 0) (unit 1)
                (uuid "22222222-2222-4222-8222-222222222222")
                (property "Reference" "U1")))"#,
            )
            .unwrap();
            let mut args = json!({"schematic":path,"connections":[{"reference":"U1","pin_number":"1","net":"SIGNAL"}]});
            args[if horizontal { "bus_y" } else { "bus_x" }] = json!(76.2001);
            let ctx = ToolContext::new(
                Default::default(),
                std::sync::Arc::new(crate::router::ToolRouter::new()),
            );
            let result = handle_connect_pins_to_bus(&args, &ctx).await.unwrap();
            assert!(!result.is_error, "{result:?}");
            let text = std::fs::read_to_string(path).unwrap();
            let root = parse_sexp(&text).unwrap();
            let wire = konnect_sexp::schematic::extract_wires(&root).remove(0);
            assert_eq!((wire.x1, wire.y1), (50.165, 50.165), "{text}");
            let labels = konnect_sexp::schematic::extract_labels(&root);
            assert_eq!((labels[0].x, labels[0].y), (50.165, 50.165));
            let entry = root.find("bus_entry").unwrap();
            let at = entry.find("at").unwrap();
            let size = entry.find("size").unwrap();
            let bus_coordinate = if horizontal {
                at.get_f64(2).unwrap() + size.get_f64(2).unwrap()
            } else {
                at.get_f64(1).unwrap() + size.get_f64(1).unwrap()
            };
            assert!(
                (bus_coordinate - 76.2).abs() < 1e-9,
                "caller bus geometry must still snap"
            );
        }
    }
}

#[cfg(test)]
mod fanout_safety_tests {
    use super::*;

    fn sheet(points: &[(&str, f64, f64)]) -> (tempfile::TempDir, std::path::PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bus-safety.kicad_sch");
        let mut text = String::from(
            r#"(kicad_sch
          (lib_symbols (symbol "Test:P" (symbol "P_1_1"
            (pin passive line (at 0 0 0) (length 0) (name "P") (number "1")))))"#,
        );
        for (index, &(reference, x, y)) in points.iter().enumerate() {
            text.push_str(&format!(
                r#"(symbol (lib_id "Test:P") (at {x} {y} 0) (unit 1)
          (uuid "22222222-2222-4222-8222-{index:012}") (property "Reference" "{reference}"))"#
            ));
        }
        text.push(')');
        std::fs::write(&path, &text).unwrap();
        (dir, path, text)
    }
    fn ctx() -> ToolContext {
        ToolContext::new(
            Default::default(),
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        )
    }
    #[tokio::test]
    async fn bus_fanout_refuses_a_foreign_pin_without_writing() {
        let (_dir, path, before) = sheet(&[("R1", 0., 0.), ("C1", 0., 5.)]);
        let result=handle_connect_pins_to_bus(&json!({"schematic":path,"bus_y":10.16,"connections":[{"reference":"R1","pin_number":"1","net":"SIGNAL"}]}),&ctx()).await.unwrap();
        assert!(result.is_error, "{result:?}");
        assert!(serde_json::to_string(&result)
            .unwrap()
            .contains("would_join_nets"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), before);
    }
    #[tokio::test]
    async fn bus_fanout_refuses_duplicate_references_without_writing() {
        let (_dir, path, before) = sheet(&[("R1", 0., 0.), ("R1", 10., 0.)]);
        let result=handle_connect_pins_to_bus(&json!({"schematic":path,"bus_y":10.16,"connections":[{"reference":"R1","pin_number":"1","net":"SIGNAL"}]}),&ctx()).await.unwrap();
        assert!(result.is_error, "{result:?}");
        assert!(serde_json::to_string(&result)
            .unwrap()
            .contains("ambiguous_reference"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), before);
    }
}
