//! Regression coverage for public inputs removed because they never affected
//! the operation. Every removal has a user-facing migration entry.

use konnect_core::router::registry;

const REMOVED: &[(&str, &str)] = &[
    ("import_sheet_pins", "project_name"),
    ("refill_zones", "zones"),
    ("run_drc", "tests"),
    ("audit_decoupling", "board"),
    ("audit_decoupling", "max_distance_mm"),
    ("export_manufacturing_package", "quantity"),
    ("validate_for_manufacturing", "schematic"),
];

fn tool(name: &str) -> konnect_core::tools::ToolDef {
    registry::ALL_TOOLSETS
        .iter()
        .flat_map(|toolset| registry::tools_for(toolset.name).unwrap_or_default())
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("registered tool {name}"))
}

#[test]
fn ignored_inputs_are_no_longer_advertised() {
    for &(tool_name, field) in REMOVED {
        let definition = tool(tool_name);
        assert!(
            definition.input_schema["properties"].get(field).is_none(),
            "{tool_name}.{field} never affected its operation and must not return"
        );
    }
}

#[test]
fn every_removed_input_has_a_migration_note() {
    let migrations = include_str!("../../../docs/API_MIGRATIONS.md");
    for &(tool_name, field) in REMOVED {
        assert!(
            migrations.contains(&format!("`{tool_name}.{field}`")),
            "missing migration for {tool_name}.{field}"
        );
    }
}

#[test]
fn estimate_cost_declares_an_optional_schematic_for_linked_catalog_fields() {
    let schema = tool("estimate_cost").input_schema;
    assert_eq!(schema["required"], serde_json::json!(["board"]));
    let input = &schema["properties"]["schematic"];
    assert_eq!(input["type"], "string");
    assert!(input["description"].as_str().unwrap().contains("LCSC"));
}

#[test]
fn narrowed_tools_describe_their_real_scope() {
    assert!(tool("audit_decoupling")
        .description
        .contains("does not inspect PCB placement distance"));
    assert!(tool("run_drc").description.contains("no per-test selector"));
    assert!(tool("refill_zones").description.contains("complete board"));
}
