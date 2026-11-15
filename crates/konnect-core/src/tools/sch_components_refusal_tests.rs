use super::*;
use crate::router::ToolRouter;
use crate::tools::ServerConfig;
use serde_json::json;
use std::sync::Arc;

fn ctx() -> ToolContext {
    ToolContext::new(ServerConfig::default(), Arc::new(ToolRouter::new()))
}

fn symbol(reference: &str, uuid: &str, extra: &str) -> String {
    format!(
        "\t(symbol\n\t\t(lib_id \"Device:R\")\n\t\t(at 50 60 0)\n\t\t(unit 1)\n{extra}\t\t(uuid \"{uuid}\")\n\t\t(property \"Reference\" \"{reference}\"\n\t\t\t(at 52 58 0)\n\t\t)\n\t\t(property \"Value\" \"10k\"\n\t\t\t(at 52 62 0)\n\t\t)\n\t\t(instances\n\t\t\t(project \"proj\"\n\t\t\t\t(path \"/root\"\n\t\t\t\t\t(reference \"{reference}\") (unit 1)\n\t\t\t\t)\n\t\t\t)\n\t\t)\n\t)\n"
    )
}

fn sheet(symbols: &[String]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.kicad_sch");
    std::fs::write(
        &path,
        format!(
            "(kicad_sch\n\t(version 20250610)\n\t(generator \"eeschema\")\n\t(uuid \"root\")\n\t(lib_symbols\n\t\t(symbol \"Device:R\"\n\t\t\t(property \"Reference\" \"R\" (at 0 0 0))\n\t\t)\n\t)\n{}\t(sheet_instances\n\t\t(path \"/\" (page \"1\"))\n\t)\n)\n",
            symbols.concat()
        ),
    )
    .unwrap();
    (dir, path)
}

fn one() -> (tempfile::TempDir, std::path::PathBuf) {
    sheet(&[symbol("R1", "sym-1", "")])
}

fn two() -> (tempfile::TempDir, std::path::PathBuf) {
    sheet(&[symbol("R1", "sym-1", ""), symbol("R2", "sym-2", "")])
}

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn text(result: &CallToolResult) -> String {
    match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
        other => panic!("expected text, got {other:?}"),
    }
}

async fn edit(path: &std::path::Path, mut args: serde_json::Value) -> CallToolResult {
    args["schematic"] = json!(path.display().to_string());
    handle_edit_schematic_component(&args, &ctx())
        .await
        .unwrap()
}

async fn refused_without_writing(path: &std::path::Path, args: serde_json::Value, needle: &str) {
    let before = read(path);
    let result = edit(path, args).await;
    assert!(result.is_error, "expected a refusal: {}", text(&result));
    assert!(
        text(&result).contains(needle),
        "{needle}: {}",
        text(&result)
    );
    assert_eq!(read(path), before, "a refused edit must not write");
}

#[tokio::test]
async fn editing_native_attributes_inserts_missing_flags_and_replaces_existing_ones() {
    let (_d, path) = one();
    let result = edit(
        &path,
        json!({"reference": "R1", "dnp": true, "in_bom": false}),
    )
    .await;
    assert!(!result.is_error, "{}", text(&result));
    let after = read(&path);
    assert!(after.contains("(dnp yes)"), "{after}");
    assert!(after.contains("(in_bom no)"), "{after}");

    let result = edit(&path, json!({"reference": "R1", "dnp": false})).await;
    assert!(!result.is_error, "{}", text(&result));
    let after = read(&path);
    assert!(
        after.contains("(dnp no)") && !after.contains("(dnp yes)"),
        "{after}"
    );
    assert_eq!(
        after.matches("(dnp ").count(),
        1,
        "no duplicate flag: {after}"
    );
    assert!(
        after.contains("(in_bom no)"),
        "untouched flags survive: {after}"
    );
}

#[tokio::test]
async fn editing_every_native_attribute_writes_each_one() {
    let (_d, path) = one();
    let result = edit(
        &path,
        json!({"reference": "R1", "on_board": false, "exclude_from_sim": true, "in_pos_files": false}),
    )
    .await;
    assert!(!result.is_error, "{}", text(&result));
    let after = read(&path);
    for flag in [
        "(on_board no)",
        "(exclude_from_sim yes)",
        "(in_pos_files no)",
    ] {
        assert!(after.contains(flag), "{flag}: {after}");
    }
}

#[tokio::test]
async fn a_non_boolean_native_attribute_is_refused_before_anything_is_written() {
    let (_d, path) = one();
    refused_without_writing(&path, json!({"reference": "R1", "dnp": "yes"}), "dnp").await;
    refused_without_writing(
        &path,
        json!({"reference": "R1", "in_pos_files": 1}),
        "in_pos_files",
    )
    .await;
}

#[tokio::test]
async fn a_custom_dnp_field_is_refused_in_favour_of_the_native_flag() {
    let (_d, path) = one();
    refused_without_writing(
        &path,
        json!({"reference": "R1", "fields": {"dnp": "1"}}),
        "native 'dnp' boolean",
    )
    .await;
}

#[tokio::test]
async fn editing_an_unknown_component_is_refused_without_writing() {
    let (_d, path) = one();
    let before = read(&path);
    let result = edit(&path, json!({"reference": "R99", "value": "1k"})).await;
    assert!(result.is_error, "{}", text(&result));
    assert!(text(&result).contains("R99"), "{}", text(&result));
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn non_string_custom_field_values_are_refused_and_named() {
    let (_d, path) = one();
    refused_without_writing(
        &path,
        json!({"reference": "R1", "fields": {"Tolerance": 5}}),
        "Tolerance: field values must be strings",
    )
    .await;
}

#[tokio::test]
async fn an_empty_fields_object_changes_nothing_and_does_not_claim_success() {
    let (_d, path) = one();
    let before = read(&path);
    let result = edit(&path, json!({"reference": "R1", "fields": {}})).await;
    assert!(!result.is_error, "{}", text(&result));
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn field_placements_move_a_field_and_report_each_invalid_entry() {
    let (_d, path) = one();
    let result = edit(
        &path,
        json!({"reference": "R1", "field_placements": {"Value": {"x": 70.0, "y": 80.0, "rotation": 90.0}}}),
    )
    .await;
    assert!(!result.is_error, "{}", text(&result));
    assert!(read(&path).contains("(at 70 80 90)"), "{}", read(&path));

    for (placements, needle) in [
        (
            json!({"Value": 3}),
            "field_placements entries must be objects",
        ),
        (
            json!({"Value": {"x": "left"}}),
            "Value.x: expected a number",
        ),
        (
            json!({"Value": {"hide": "yes"}}),
            "Value.hide: expected true or false",
        ),
        (json!({"Value": {}}), "sets nothing"),
        (json!({"Value": {"x": null, "hide": null}}), "sets nothing"),
    ] {
        let before = read(&path);
        let result = edit(
            &path,
            json!({"reference": "R1", "field_placements": placements}),
        )
        .await;
        assert!(result.is_error, "{needle}: {}", text(&result));
        assert!(
            text(&result).contains(needle),
            "{needle}: {}",
            text(&result)
        );
        assert_eq!(read(&path), before);
    }
}

#[tokio::test]
async fn field_placement_unit_zero_is_an_invalid_argument() {
    let (_d, path) = one();
    refused_without_writing(
        &path,
        json!({"reference": "R1", "unit": 0, "field_placements": {"Value": {"x": 1.0}}}),
        "unit",
    )
    .await;
}

#[tokio::test]
async fn field_placements_can_hide_a_field_without_moving_it() {
    let (_d, path) = one();
    let result = edit(
        &path,
        json!({"reference": "R1", "field_placements": {"Value": {"hide": true}}}),
    )
    .await;
    assert!(!result.is_error, "{}", text(&result));
    assert!(read(&path).contains("(hide yes)"), "{}", read(&path));
}

#[tokio::test]
async fn annotation_refuses_built_in_fields_and_unknown_components() {
    let (_d, path) = one();
    let before = read(&path);
    for key in ["Reference", "Value", "Footprint", "Datasheet"] {
        let result = handle_add_component_annotation(
            &json!({"schematic": path.display().to_string(), "reference": "R1", "key": key, "value": "x"}),
            &ctx(),
        )
        .await
        .unwrap();
        assert!(result.is_error, "{key}");
        assert!(
            text(&result).contains("is a built-in field"),
            "{}",
            text(&result)
        );
    }
    let result = handle_add_component_annotation(
        &json!({"schematic": path.display().to_string(), "reference": "R99", "key": "MPN", "value": "x"}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(result.is_error, "{}", text(&result));
    for missing in ["reference", "key", "value"] {
        let mut args = json!({"schematic": path.display().to_string(), "reference": "R1", "key": "MPN", "value": "x"});
        args.as_object_mut().unwrap().remove(missing);
        let result = handle_add_component_annotation(&args, &ctx())
            .await
            .unwrap();
        assert!(result.is_error, "missing {missing}");
    }
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn annotation_adds_a_new_key_then_updates_it_in_place() {
    let (_d, path) = one();
    let args = |value: &str| json!({"schematic": path.display().to_string(), "reference": "R1", "key": "MPN", "value": value});
    let first = handle_add_component_annotation(&args("A"), &ctx())
        .await
        .unwrap();
    assert!(!first.is_error, "{}", text(&first));
    let body: serde_json::Value = serde_json::from_str(&text(&first)).unwrap();
    assert_eq!(body["updated_existing"], false);
    assert_eq!(body["added_units"], 1);

    let second = handle_add_component_annotation(&args("B"), &ctx())
        .await
        .unwrap();
    assert!(!second.is_error, "{}", text(&second));
    let body: serde_json::Value = serde_json::from_str(&text(&second)).unwrap();
    assert_eq!(body["updated_existing"], true);
    assert_eq!(read(&path).matches("(property \"MPN\"").count(), 1);
    assert!(read(&path).contains("(property \"MPN\" \"B\""));
}

#[tokio::test]
async fn grouping_tags_every_named_component_and_reports_unknown_ones() {
    let (_d, path) = two();
    let result = handle_group_components(
        &json!({"schematic": path.display().to_string(), "group_name": "Power",
                "references": ["R1", "R2", "R1", "R404"]}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{}", text(&result));
    let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
    assert_eq!(body["grouped_count"], 2, "a repeated reference counts once");
    assert!(
        body["errors"][0].as_str().unwrap().contains("R404"),
        "{body}"
    );
    assert_eq!(
        read(&path).matches("(property \"Group\" \"Power\"").count(),
        2
    );
}

#[tokio::test]
async fn grouping_refuses_empty_or_wholly_unusable_selections_without_writing() {
    let (_d, path) = two();
    let before = read(&path);
    let schematic = path.display().to_string();

    let empty = handle_group_components(
        &json!({"schematic": schematic, "group_name": "G", "references": []}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(empty.is_error && text(&empty).contains("No references provided"));

    let unusable = handle_group_components(
        &json!({"schematic": schematic, "group_name": "G", "references": [7, "R404"]}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(unusable.is_error, "{}", text(&unusable));
    assert!(
        text(&unusable).contains("must be a string"),
        "{}",
        text(&unusable)
    );
    assert!(text(&unusable).contains("R404"), "{}", text(&unusable));

    for missing in ["group_name", "references"] {
        let mut args = json!({"schematic": schematic, "group_name": "G", "references": ["R1"]});
        args.as_object_mut().unwrap().remove(missing);
        let result = handle_group_components(&args, &ctx()).await.unwrap();
        assert!(result.is_error, "missing {missing}");
    }
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn updating_from_library_refuses_a_component_that_is_not_on_the_sheet() {
    let (_d, path) = one();
    let before = read(&path);
    let result = handle_update_symbols_from_library(
        &json!({"schematic": path.display().to_string(), "references": ["R1", "U404"]}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(result.is_error, "{}", text(&result));
    assert!(
        text(&result).contains("Component 'U404' not found"),
        "{}",
        text(&result)
    );
    assert_eq!(read(&path), before);
}
