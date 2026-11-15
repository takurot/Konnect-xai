use super::*;
use crate::router::ToolRouter;
use crate::tools::ServerConfig;
use std::sync::Arc;

fn ctx() -> ToolContext {
    ToolContext::new(ServerConfig::default(), Arc::new(ToolRouter::new()))
}

fn text(result: &CallToolResult) -> String {
    match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
        other => panic!("expected text, got {other:?}"),
    }
}

fn refusal(result: Result<SymbolItems, CallToolResult>) -> String {
    match result {
        Ok(_) => panic!("expected a refusal"),
        Err(error) => {
            assert!(error.is_error);
            text(&error)
        }
    }
}

#[test]
fn symbol_items_default_to_empty_and_accept_well_formed_pins() {
    let items = parse_symbol_items(&json!({})).unwrap();
    assert!(items.pins.is_empty() && items.units.is_empty() && items.power_pins.is_empty());
    let items = parse_symbol_items(&json!({
        "pins": [{"number": "1", "name": "A", "type": "input"}],
        "units": [{"pins": [{"number": "2", "name": "B", "type": "output"}]}],
        "power_pins": [{"number": "3", "name": "VCC", "type": "power_in", "x": 0.0, "y": 1.0}]
    }))
    .unwrap();
    assert_eq!(
        (items.pins.len(), items.units.len(), items.power_pins.len()),
        (1, 1, 1)
    );
}

#[test]
fn malformed_symbol_items_are_refused_with_the_indexed_path() {
    let pin = json!({"number": "1", "name": "A", "type": "input"});
    let cases = [
        (json!({"pins": "x"}), "pins"),
        (json!({"pins": [3]}), "pins[0]"),
        (
            json!({"pins": [{"name": "A", "type": "input"}]}),
            "pins[0].number",
        ),
        (
            json!({"pins": [pin.clone(), {"number": "2", "name": 5, "type": "input"}]}),
            "pins[1].name",
        ),
        (json!({"units": [3]}), "units[0]"),
        (json!({"units": [{}]}), "units[0].pins"),
        (
            json!({"units": [{"pins": [{"number": "1"}]}]}),
            "units[0].pins[0].name",
        ),
        (json!({"power_pins": [pin.clone()]}), "power_pins[0].x"),
        (
            json!({"power_pins": [{"number": "1", "name": "V", "type": "power_in", "x": 0.0}]}),
            "power_pins[0].y",
        ),
    ];
    for (args, path) in cases {
        let message = refusal(parse_symbol_items(&args));
        assert!(message.contains(path), "{path}: {message}");
    }
}

#[test]
fn pin_electrical_types_outside_kicads_vocabulary_are_refused_with_a_hint() {
    assert!(validate_pin_types(&[json!({"type": "input"}), json!({})]).is_ok());
    let error = validate_pin_types(&[json!({"number": "7", "type": "not_connected"})])
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("pin \"7\"") && error.contains("did you mean \"no_connect\""),
        "{error}"
    );
    let error = validate_pin_types(&[json!({"type": "sideways"})])
        .unwrap_err()
        .to_string();
    assert!(!error.contains("did you mean"), "{error}");
}

#[test]
fn drawn_bodies_need_explicit_pin_coordinates() {
    assert!(validate_drawn_pin_coordinates(&[json!({"x": 0.0, "y": 1.0})], "pins").is_ok());
    let error = validate_drawn_pin_coordinates(&[json!({"x": 0.0})], "pins").unwrap_err();
    assert!(text(&error).contains("pins[0].y"), "{}", text(&error));
    let error = validate_drawn_pin_coordinates(&[json!({"y": 0.0})], "units[1].pins").unwrap_err();
    assert!(
        text(&error).contains("units[1].pins[0].x"),
        "{}",
        text(&error)
    );
}

fn point(x: f64, y: f64) -> serde_json::Value {
    json!({"x": x, "y": y})
}

#[test]
fn well_formed_graphics_are_accepted() {
    assert!(validate_graphics(&[
        json!({"type": "line", "start": point(0.0, 0.0), "end": point(1.0, 1.0), "stroke_width_mm": 0.0}),
        json!({"type": "arc", "start": point(0.0, 0.0), "mid": point(1.0, 1.0), "end": point(2.0, 0.0), "stroke_width_mm": 0.2}),
        json!({"type": "circle", "center": point(0.0, 0.0), "radius_mm": 1.0, "stroke_width_mm": 0.2, "fill": "none"}),
        json!({"type": "rect", "start": point(0.0, 0.0), "end": point(1.0, 1.0), "stroke_width_mm": 0.2, "fill": "background"}),
        json!({"type": "poly", "points": [point(0.0, 0.0), point(1.0, 0.0), point(0.0, 1.0)], "stroke_width_mm": 0.2, "fill": "outline"}),
    ])
    .is_ok());
}

#[test]
fn malformed_graphics_are_refused_naming_the_primitive() {
    let line = |extra: serde_json::Value| {
        let mut value = json!({"type": "line", "start": point(0.0, 0.0), "end": point(1.0, 1.0), "stroke_width_mm": 0.1});
        for (k, v) in extra.as_object().unwrap() {
            value[k] = v.clone();
        }
        value
    };
    let cases = [
        (json!(3), "each entry must be an object"),
        (json!({"type": "blob"}), "unknown type \"blob\""),
        (line(json!({"fill": "none"})), "takes no 'fill'"),
        (
            line(json!({"colour": "red"})),
            "colour (line): unknown field",
        ),
        (
            line(json!({"end": {"x": 1.0}})),
            "'end' must be an object with numeric x and y",
        ),
        (line(json!({"stroke_width_mm": -1.0})), "cannot be negative"),
        (
            line(json!({"stroke_width_mm": "thick"})),
            "'stroke_width_mm' is required",
        ),
        (
            json!({"type": "circle", "center": point(0.0, 0.0), "radius_mm": 0.0, "stroke_width_mm": 0.1, "fill": "none"}),
            "greater than 0",
        ),
        (
            json!({"type": "circle", "center": point(0.0, 0.0), "stroke_width_mm": 0.1, "fill": "none"}),
            "'radius_mm' is required",
        ),
        (
            json!({"type": "poly", "points": [point(0.0, 0.0), point(1.0, 1.0)], "stroke_width_mm": 0.1, "fill": "none"}),
            "at least 3 entries, got 2",
        ),
        (
            json!({"type": "poly", "points": [point(0.0, 0.0), point(1.0, 1.0), {"x": 1.0}], "stroke_width_mm": 0.1, "fill": "none"}),
            "points[2] must have numeric x and y",
        ),
        (
            json!({"type": "rect", "start": point(0.0, 0.0), "end": point(1.0, 1.0), "stroke_width_mm": 0.1, "fill": "plaid"}),
            "fill",
        ),
    ];
    for (graphic, needle) in cases {
        let error = validate_graphics(std::slice::from_ref(&graphic)).unwrap_err();
        assert!(error.contains(needle), "{graphic}: {error}");
    }
}

async fn create(args: serde_json::Value) -> anyhow::Result<CallToolResult> {
    handle_create_footprint(&args, &ctx()).await
}

fn pad(number: &str) -> serde_json::Value {
    json!({"number": number, "type": "smd", "shape": "rect", "x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0})
}

#[tokio::test]
async fn created_footprints_honour_layers_rotation_drill_and_roundrect_ratio() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("nested/F.kicad_mod");
    let result = create(json!({
        "output": output.display().to_string(),
        "name": "F",
        "pads": [
            {"number": "1", "type": "smd", "shape": "roundrect", "x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0,
             "rotation": 90.0, "roundrect_rratio": 0.25, "layers": ["F.Cu", "F.Mask"]},
            {"number": "2", "type": "thru_hole", "shape": "circle", "x": 2.0, "y": 0.0, "width": 1.6, "height": 1.6, "drill": 0.8}
        ]
    }))
    .await
    .unwrap();
    assert!(!result.is_error, "{}", text(&result));
    let written = std::fs::read_to_string(&output).unwrap();
    assert!(
        written.contains("(at 0 0 90)") && written.contains("(layers \"F.Cu\" \"F.Mask\")"),
        "{written}"
    );
    assert!(
        written.contains("(roundrect_rratio 0.25)") && written.contains("(drill 0.8)"),
        "{written}"
    );
    assert!(
        written.contains("(layers \"*.Cu\" \"*.Mask\")"),
        "through-hole default layers: {written}"
    );
    assert!(written.contains("(attr smd)"), "{written}");
}

#[tokio::test]
async fn a_through_hole_only_footprint_is_attributed_through_hole_and_padless_has_no_graphics() {
    let dir = tempfile::tempdir().unwrap();
    let th = dir.path().join("TH.kicad_mod");
    create(json!({"output": th.display().to_string(), "name": "TH", "pads": [
        {"number": "1", "type": "thru_hole", "shape": "circle", "x": 0.0, "y": 0.0, "width": 1.6, "height": 1.6, "drill": 0.8}
    ]}))
    .await
    .unwrap();
    assert!(std::fs::read_to_string(&th)
        .unwrap()
        .contains("(attr through_hole)"));

    let empty = dir.path().join("Empty.kicad_mod");
    let result =
        create(json!({"output": empty.display().to_string(), "name": "Empty", "pads": []}))
            .await
            .unwrap();
    let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
    assert_eq!(body["pad_count"], 0);
    assert_eq!(body["courtyard"], false);
}

#[tokio::test]
async fn invalid_footprint_input_is_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("Bad.kicad_mod");
    let base = |pads: serde_json::Value| json!({"output": output.display().to_string(), "name": "Bad", "pads": pads});
    let mut missing_x = pad("1");
    missing_x.as_object_mut().unwrap().remove("x");
    let mut missing_number = pad("1");
    missing_number.as_object_mut().unwrap().remove("number");
    let mut bad_layer = pad("1");
    bad_layer["layers"] = json!(["Edge.Cuts"]);
    let mut empty_layers = pad("1");
    empty_layers["layers"] = json!([]);
    let mut text_layer = pad("1");
    text_layer["layers"] = json!([3]);
    let mut bad_ratio = pad("1");
    bad_ratio["roundrect_rratio"] = json!(0.9);

    for (args, needle) in [
        (base(json!([3])), "pads[0]"),
        (base(json!([missing_number])), "pads[0].number"),
        (base(json!([missing_x])), "pads[0].x"),
        (base(json!([bad_layer])), "invalid pad layer: Edge.Cuts"),
        (base(json!([empty_layers])), "must not be empty"),
        (base(json!([text_layer])), "must contain strings"),
        (base(json!([bad_ratio])), "between 0 and 0.5"),
        (
            json!({"output": output.display().to_string(), "name": "Bad", "pads": [], "overwrite": "yes"}),
            "overwrite",
        ),
    ] {
        let outcome = create(args.clone()).await;
        let message = match outcome {
            Ok(result) => {
                assert!(result.is_error, "{args}");
                text(&result)
            }
            Err(error) => error.to_string(),
        };
        assert!(message.contains(needle), "{needle}: {message}");
        assert!(!output.exists(), "{args} must not write");
    }
    for missing in ["output", "name", "pads"] {
        let mut args = base(json!([pad("1")]));
        args.as_object_mut().unwrap().remove(missing);
        let outcome = create(args).await;
        assert!(
            outcome.map_or(true, |result| result.is_error),
            "missing {missing}"
        );
    }
}

#[tokio::test]
async fn an_existing_footprint_is_only_replaced_when_overwrite_is_true() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("Keep.kicad_mod");
    std::fs::write(&output, "(footprint \"Old\")").unwrap();
    let args = |overwrite: serde_json::Value| json!({"output": output.display().to_string(), "name": "New", "pads": [pad("1")], "overwrite": overwrite});
    let refused = create(args(json!(false))).await.unwrap();
    assert!(
        refused.is_error && text(&refused).contains("already exists"),
        "{}",
        text(&refused)
    );
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "(footprint \"Old\")"
    );

    let replaced = create(args(json!(true))).await.unwrap();
    assert!(!replaced.is_error, "{}", text(&replaced));
    assert!(std::fs::read_to_string(&output)
        .unwrap()
        .contains("(footprint \"New\""));
}

fn project_with_table(file: &str, body: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(file), body).unwrap();
    let project = dir.path().join("p.kicad_pro");
    (dir, project.display().to_string())
}

#[tokio::test]
async fn project_scope_lists_only_the_project_table_and_tags_each_entry() {
    let (_d, project) = project_with_table(
        "fp-lib-table",
        "(fp_lib_table (lib (name \"A\") (type \"KiCad\") (uri \"/a.pretty\")) (lib (name \"B\") (type \"KiCad\") (uri \"/b.pretty\")))",
    );
    let result =
        handle_list_footprint_libraries(&json!({"scope": "project", "project": project}), &ctx())
            .await
            .unwrap();
    let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
    assert_eq!(body["count"], 2);
    assert!(body["libraries"]
        .as_array()
        .unwrap()
        .iter()
        .all(|lib| lib["scope"] == "project"));

    let without_project = handle_list_footprint_libraries(&json!({"scope": "project"}), &ctx())
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&text(&without_project)).unwrap();
    assert_eq!(body["count"], 0, "no project given, nothing to list");
}

#[tokio::test]
async fn symbol_listing_does_the_same_and_reports_an_unreadable_table() {
    let (_d, project) = project_with_table(
        "sym-lib-table",
        "(sym_lib_table (lib (name \"S\") (type \"KiCad\") (uri \"/s.kicad_sym\")))",
    );
    let result =
        handle_list_symbol_libraries(&json!({"scope": "project", "project": project}), &ctx())
            .await
            .unwrap();
    let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
    assert_eq!(body["count"], 1);
    assert_eq!(body["libraries"][0]["scope"], "project");

    let unreadable = tempfile::tempdir().unwrap();
    std::fs::create_dir(unreadable.path().join("sym-lib-table")).unwrap();
    let project = unreadable.path().join("p.kicad_pro").display().to_string();
    let result =
        handle_list_symbol_libraries(&json!({"scope": "project", "project": project}), &ctx())
            .await
            .unwrap();
    assert!(
        result.is_error,
        "an unreadable table must not read as zero libraries"
    );
    assert!(
        text(&result).contains("Cannot read lib-table"),
        "{}",
        text(&result)
    );

    let result =
        handle_list_footprint_libraries(&json!({"scope": "project", "project": project}), &ctx())
            .await
            .unwrap();
    assert!(!result.is_error, "an absent fp-lib-table is simply empty");
}
