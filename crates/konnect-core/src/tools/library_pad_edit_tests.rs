use super::*;
use crate::router::ToolRouter;
use crate::tools::ServerConfig;
use std::sync::Arc;

const FOOTPRINT: &str = concat!(
    "(footprint \"X\" (version 20240108) (generator \"pcbnew\")\n",
    "  (layer \"F.Cu\")\n",
    "  (pad \"1\" smd rect (at 1 2 90) (size 1 1.5) (layers \"F.Cu\" \"F.Paste\" \"F.Mask\"))\n",
    "  (pad \"2\" thru_hole circle (at 3 4) (size 1.6 1.6) (drill 0.8) (layers \"*.Cu\"))\n",
    "  (pad \"2\" smd rect (at 5 6) (size 1 1) (layers \"F.Cu\"))\n",
    "  (pad \"3\" smd roundrect\n",
    "    (at 7 8)\n",
    "    (size 2 2)\n",
    "    (roundrect_rratio 0.1)\n",
    "    (layers \"F.Cu\"))\n",
    "  (pad \"4\" smd rect\n",
    "    (at 9 10)\n",
    "    (size 2 2)\n",
    "    (layers \"F.Cu\"))\n",
    ")\n"
);

fn ctx() -> ToolContext {
    ToolContext::new(ServerConfig::default(), Arc::new(ToolRouter::new()))
}

fn footprint_file(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("X.kicad_mod");
    std::fs::write(&path, content).unwrap();
    (dir, path)
}

fn text(result: &CallToolResult) -> String {
    match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
        other => panic!("expected text, got {other:?}"),
    }
}

async fn edit(path: &std::path::Path, mut args: serde_json::Value) -> CallToolResult {
    args["footprint_path"] = json!(path.display().to_string());
    handle_edit_footprint_pad(&args, &ctx()).await.unwrap()
}

async fn edited(args: serde_json::Value) -> String {
    let (_d, path) = footprint_file(FOOTPRINT);
    let result = edit(&path, args).await;
    assert!(!result.is_error, "{}", text(&result));
    std::fs::read_to_string(&path).unwrap()
}

#[tokio::test]
async fn moving_one_axis_keeps_the_other_and_the_rotation() {
    let after = edited(json!({"pad_number": "1", "x": 10.5})).await;
    assert!(after.contains("(at 10.5 2 90)"), "{after}");
    let after = edited(json!({"pad_number": "1", "y": -3.25})).await;
    assert!(after.contains("(at 1 -3.25 90)"), "{after}");
}

#[tokio::test]
async fn resizing_one_dimension_keeps_the_other() {
    let after = edited(json!({"pad_number": "1", "width": 2.5})).await;
    assert!(after.contains("(size 2.5 1.5)"), "{after}");
    let after = edited(json!({"pad_number": "1", "height": 0.75})).await;
    assert!(after.contains("(size 1 0.75)"), "{after}");
}

#[tokio::test]
async fn drill_is_replaced_when_present_and_added_when_absent() {
    let after = edited(json!({"pad_number": "1", "drill": 0.6})).await;
    assert!(
        after.contains("(layers \"F.Cu\" \"F.Paste\" \"F.Mask\") (drill 0.6))"),
        "{after}"
    );
    let after = edited(json!({"pad_number": "2", "drill": 1.1})).await;
    assert!(
        after.contains("(drill 1.1)") && !after.contains("(drill 0.8)"),
        "{after}"
    );
}

#[tokio::test]
async fn renumbering_changes_only_the_first_matching_pad_unless_match_all_is_set() {
    let after = edited(json!({"pad_number": "2", "new_number": "9"})).await;
    assert_eq!(after.matches("(pad \"9\"").count(), 1, "{after}");
    assert_eq!(after.matches("(pad \"2\"").count(), 1, "{after}");

    let after = edited(json!({"pad_number": "2", "new_number": "9", "match_all": true})).await;
    assert_eq!(after.matches("(pad \"9\"").count(), 2, "{after}");
    assert_eq!(after.matches("(pad \"2\"").count(), 0, "{after}");
}

#[tokio::test]
async fn changing_to_roundrect_adds_a_ratio_in_the_pads_own_layout() {
    let after = edited(json!({"pad_number": "1", "shape": "roundrect"})).await;
    assert!(after.contains("smd roundrect (at 1 2 90)"), "{after}");
    assert!(
        after.contains(" (roundrect_rratio 0.25))"),
        "inline pad: {after}"
    );

    let after = edited(json!({"pad_number": "4", "shape": "roundrect"})).await;
    assert!(
        after.contains("\n    (roundrect_rratio 0.25)"),
        "multi-line pad: {after}"
    );
}

#[tokio::test]
async fn leaving_roundrect_drops_the_ratio_and_keeping_it_does_not_duplicate_it() {
    let after = edited(json!({"pad_number": "3", "shape": "oval"})).await;
    assert!(!after.contains("(roundrect_rratio 0.1)"), "{after}");
    let after = edited(json!({"pad_number": "3", "shape": "roundrect"})).await;
    assert_eq!(after.matches("roundrect_rratio").count(), 1, "{after}");
    assert!(after.contains("(roundrect_rratio 0.1)"), "{after}");
}

#[tokio::test]
async fn a_zone_connection_is_reported_as_read_back_from_the_written_pad() {
    let (_d, path) = footprint_file(FOOTPRINT);
    let result = edit(&path, json!({"pad_number": "1", "zone_connect": "thermal"})).await;
    assert!(!result.is_error, "{}", text(&result));
    let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
    assert_eq!(body["zone_connect"], "thermal");
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("(zone_connect"));
}

#[tokio::test]
async fn every_invalid_pad_edit_argument_is_refused_without_writing() {
    let cases = [
        (json!({"pad_number": "1", "new_number": 5}), "new_number"),
        (json!({"pad_number": "1", "match_all": "yes"}), "match_all"),
        (
            json!({"pad_number": "1", "shape": "hexagon"}),
            "unsupported standard shape 'hexagon'",
        ),
        (
            json!({"pad_number": "1", "shape": 3}),
            "must be a string when supplied",
        ),
        (
            json!({"pad_number": "1", "zone_connect": "sideways"}),
            "unsupported zone connection 'sideways'",
        ),
        (
            json!({"pad_number": "1", "zone_connect": 1}),
            "zone_connect",
        ),
        (
            json!({"pad_number": "77", "x": 1.0}),
            "pad '77' was not found",
        ),
    ];
    for (args, needle) in cases {
        let (_d, path) = footprint_file(FOOTPRINT);
        let result = edit(&path, args.clone()).await;
        assert!(result.is_error, "{args}: {}", text(&result));
        assert!(
            text(&result).contains(needle),
            "{needle}: {}",
            text(&result)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FOOTPRINT, "{args}");
    }
    let (_d, path) = footprint_file(FOOTPRINT);
    let result = handle_edit_footprint_pad(
        &json!({"footprint_path": path.display().to_string(), "x": 1.0}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(result.is_error, "pad_number is required");
}

#[tokio::test]
async fn a_file_that_is_not_a_footprint_is_refused_untouched() {
    for (content, needle) in [
        ("(kicad_sch (version 1))", "footprint_path"),
        ("(footprint \"X\" (pad \"1\"", "malformed"),
    ] {
        let (_d, path) = footprint_file(content);
        let result = edit(&path, json!({"pad_number": "1", "x": 1.0})).await;
        assert!(result.is_error, "{content}: {}", text(&result));
        assert!(
            text(&result).contains(needle),
            "{needle}: {}",
            text(&result)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
    }
}
