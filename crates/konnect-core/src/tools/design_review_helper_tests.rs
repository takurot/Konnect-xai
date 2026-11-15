use super::*;
use crate::mcp::protocol::ToolContent;
use konnect_sexp::schematic::SymbolInstance;

fn instance(reference: &str, value: &str) -> SymbolInstance {
    let source = format!(
        "(kicad_sch (symbol (lib_id \"Device:X\") (at 0 0 0) (unit 1) (uuid \"u\") (property \"Reference\" \"{reference}\" (at 0 0 0)) (property \"Value\" \"{value}\" (at 0 0 0))))"
    );
    let tree = parse_sexp(&source).unwrap();
    konnect_sexp::schematic::extract_symbol_instances(&tree)
        .into_iter()
        .next()
        .expect("one instance")
}

#[test]
fn power_pin_names_match_supply_prefixes_case_insensitively() {
    for name in [
        "vcc",
        "VDD_IO",
        "VBUS",
        "V+",
        "VIN",
        "3V3",
        "5V",
        "1V8",
        "2V5",
        "AVCC",
        "avdd",
        "DVCC",
        "DVDD",
        "VCAP_1",
        "VREF+",
        "MAIN_POWER",
        "PWR_OK",
    ] {
        assert!(is_power_pin_name(name), "{name} is a supply pin");
    }
    for name in ["GND", "PA0", "SDA", "AVCC2", "OUT", ""] {
        assert!(!is_power_pin_name(name), "{name} is not a supply pin");
    }
}

#[test]
fn power_net_names_cover_rails_and_grounds_but_not_signals() {
    for name in [
        "VCC", "vdd", "3V3", "5V", "12V", "VBUS", "GND", "dgnd", "AGND", "+3V3", "V+",
    ] {
        assert!(is_power_net_name(name), "{name} is a power net");
    }
    for name in ["SIG", "SDA", "GNDX_NOT", "NRST", ""] {
        assert!(!is_power_net_name(name), "{name} is not a power net");
    }
}

#[test]
fn bulk_capacitor_values_start_at_ten_microfarad() {
    for value in ["10uF", "22u", "47UF", "100uF", "220u", "470uF", "1000uF"] {
        assert!(is_bulk_capacitor_value(value), "{value} is bulk");
    }
    for value in ["100nF", "1uF", "4.7u", "~", ""] {
        assert!(!is_bulk_capacitor_value(value), "{value} is not bulk");
    }
}

#[test]
fn a_capacitor_is_a_c_reference_but_not_a_cn_connector() {
    assert!(is_capacitor(&instance("C1", "")));
    assert!(is_capacitor(&instance("C100", "")));
    assert!(!is_capacitor(&instance("CN1", "")));
    assert!(!is_capacitor(&instance("R1", "")));
}

#[test]
fn design_rule_values_parse_or_report_none() {
    let content = "(rules (min_trace_width 0.127) (min_x abc) (min_y 0.2";
    assert_eq!(
        find_design_rule_value(content, "min_trace_width"),
        Some(0.127)
    );
    assert_eq!(find_design_rule_value(content, "min_x"), None);
    assert_eq!(find_design_rule_value(content, "min_y"), None);
    assert_eq!(find_design_rule_value(content, "absent"), None);
}

fn footprint(reference: &str, x: f64, y: f64) -> String {
    format!("(footprint \"F\" (at {x} {y}) (property \"Reference\" \"{reference}\"))")
}

#[test]
fn silkscreen_overlap_flags_only_footprints_closer_than_one_millimetre() {
    let board = format!(
        "(kicad_pcb {} {} {})",
        footprint("R1", 10.0, 10.0),
        footprint("R2", 10.5, 10.0),
        footprint("R3", 20.0, 10.0),
    );
    let tree = parse_sexp(&board).unwrap();
    let findings = check_silkscreen_overlap(&board, &tree);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].component.as_deref(), Some("R1, R2"));
    assert!(
        findings[0].issue.contains("0.50mm"),
        "{}",
        findings[0].issue
    );
}

#[test]
fn silkscreen_overlap_skips_footprints_without_a_reference_or_position() {
    let board =
        "(kicad_pcb (footprint \"F\" (at 1 1)) (footprint \"G\" (property \"Reference\" \"R1\")))";
    let tree = parse_sexp(board).unwrap();
    assert!(check_silkscreen_overlap(board, &tree).is_empty());
}

fn result_with(content: Vec<ToolContent>, is_error: bool) -> CallToolResult {
    CallToolResult { content, is_error }
}

fn text_result(text: &str, is_error: bool) -> CallToolResult {
    result_with(
        vec![ToolContent::Text {
            text: text.to_string(),
        }],
        is_error,
    )
}

#[test]
fn extract_findings_accepts_an_array_of_objects() {
    let findings =
        extract_findings(&text_result(r#"{"findings":[{"issue":"x"}]}"#, false)).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["issue"], "x");
}

#[test]
fn extract_findings_rejects_every_malformed_audit_result() {
    let image = result_with(
        vec![ToolContent::Image {
            data: String::new(),
            mime_type: "image/png".into(),
        }],
        false,
    );
    assert!(extract_findings(&image).unwrap_err().contains("non-text"));
    let empty = result_with(vec![], false);
    assert!(extract_findings(&empty).unwrap_err().contains("no content"));
    let errored = text_result(r#"{"findings":[]}"#, true);
    assert!(extract_findings(&errored)
        .unwrap_err()
        .contains("error result"));
    let not_json = text_result("nope", false);
    assert!(extract_findings(&not_json)
        .unwrap_err()
        .contains("not valid JSON"));
    let no_array = text_result(r#"{"findings":3}"#, false);
    assert!(extract_findings(&no_array)
        .unwrap_err()
        .contains("findings array"));
    let scalar = text_result(r#"{"findings":[1]}"#, false);
    assert!(extract_findings(&scalar)
        .unwrap_err()
        .contains("JSON objects"));
}

const BOM_SHEET: &str = r##"(kicad_sch
  (version 20250610)
  (generator "konnect")
  (uuid "11111111-1111-4111-8111-111111111111")
  (paper "A4")
  (lib_symbols)
  (symbol (lib_id "Device:R") (at 20 20 0) (unit 1)
    (uuid "a0000000-0000-4000-8000-000000000001")
    (property "Reference" "R1" (at 20 20 0))
    (property "Value" "~" (at 20 20 0))
    (property "Footprint" "" (at 20 20 0)))
  (symbol (lib_id "Device:U") (at 40 20 0) (unit 1)
    (uuid "a0000000-0000-4000-8000-000000000002")
    (property "Reference" "U1" (at 40 20 0))
    (property "Value" "MCU" (at 40 20 0))
    (property "Footprint" "Package_QFP:LQFP-48" (at 40 20 0)))
  (symbol (lib_id "Device:U") (at 60 20 0) (unit 1)
    (uuid "a0000000-0000-4000-8000-000000000003")
    (property "Reference" "U2" (at 60 20 0))
    (property "Value" "LDO" (at 60 20 0))
    (property "Footprint" "Package_TO_SOT_SMD:SOT-23" (at 60 20 0))
    (property "LCSC" "C12345" (at 60 20 0)))
  (symbol (lib_id "power:GND") (at 80 20 0) (unit 1)
    (uuid "a0000000-0000-4000-8000-000000000004")
    (property "Reference" "#PWR01" (at 80 20 0))
    (property "Value" "GND" (at 80 20 0)))
)
"##;

#[tokio::test]
async fn bom_health_counts_each_missing_field_and_skips_power_symbols() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bom.kicad_sch");
    std::fs::write(&path, BOM_SHEET).unwrap();
    let result = handle_check_bom_health_file(
        &json!({"schematic": path.display().to_string()}),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!result.is_error);
    let ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text");
    };
    let body: Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["total_components"], 3, "#PWR01 is not a component");
    assert_eq!(body["missing_value"], 1);
    assert_eq!(body["missing_footprint"], 1);
    assert_eq!(body["missing_mpn"], 1, "U2 has an LCSC part number");
    let findings = body["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 3);
    assert!(findings
        .iter()
        .any(|f| f["component"] == "U1" && f["issue"].as_str().unwrap().contains("no MPN")));
    assert!(!findings.iter().any(|f| f["component"] == "U2"));
}

#[tokio::test]
async fn bom_health_refuses_a_schematic_it_cannot_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.kicad_sch");
    let outcome = handle_check_bom_health_file(
        &json!({"schematic": path.display().to_string()}),
        &test_ctx(),
    )
    .await;
    let message = outcome
        .map(|r| format!("{:?}", r.content))
        .unwrap_or_else(|e| e.to_string());
    assert!(message.contains("Failed to load schematic"), "{message}");
}
