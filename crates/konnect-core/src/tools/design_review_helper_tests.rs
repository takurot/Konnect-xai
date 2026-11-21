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

fn footprint(reference: &str, x: f64, y: f64) -> String {
    format!("(footprint \"F\" (at {x} {y}) (property \"Reference\" \"{reference}\"))")
}

#[test]
fn origin_proximity_flags_only_footprints_closer_than_one_millimetre() {
    let board = format!(
        "(kicad_pcb {} {} {})",
        footprint("R1", 10.0, 10.0),
        footprint("R2", 10.5, 10.0),
        footprint("R3", 20.0, 10.0),
    );
    let tree = parse_sexp(&board).unwrap();
    let findings = check_footprint_origin_proximity(&tree);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].component.as_deref(), Some("R1, R2"));
    assert!(
        findings[0].issue.contains("0.50mm"),
        "{}",
        findings[0].issue
    );
}

#[test]
fn origin_proximity_skips_footprints_without_a_reference_or_position() {
    let board =
        "(kicad_pcb (footprint \"F\" (at 1 1)) (footprint \"G\" (property \"Reference\" \"R1\")))";
    let tree = parse_sexp(board).unwrap();
    assert!(check_footprint_origin_proximity(&tree).is_empty());
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
    for (check_passives, expected_missing_mpn) in [(true, 2), (false, 1)] {
        let mut args = json!({"schematic": path});
        if !check_passives {
            args["check_passive_mpn"] = json!(false);
        }
        let result = handle_check_bom_health_file(&args, &test_ctx())
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
        assert_eq!(body["missing_mpn"], expected_missing_mpn);
        let findings = body["findings"].as_array().unwrap();
        assert_eq!(findings.len(), expected_missing_mpn + 2);
        let needs_mpn = |reference: &str| {
            findings.iter().any(|finding| {
                finding["component"] == reference
                    && finding["issue"].as_str().unwrap().contains("no MPN")
            })
        };
        assert_eq!(needs_mpn("R1"), check_passives);
        assert!(needs_mpn("U1"));
        assert!(!findings.iter().any(|finding| finding["component"] == "U2"));
    }
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

#[test]
fn schematic_scope_defaults_to_file_and_refuses_anything_else() {
    assert_eq!(requested_schematic_scope(&json!({})).unwrap(), "file");
    assert_eq!(
        requested_schematic_scope(&json!({"schematic_scope": null})).unwrap(),
        "file"
    );
    assert_eq!(
        requested_schematic_scope(&json!({"schematic_scope": "file"})).unwrap(),
        "file"
    );
    assert_eq!(
        requested_schematic_scope(&json!({"schematic_scope": "hierarchy"})).unwrap(),
        "hierarchy"
    );
    let unknown = requested_schematic_scope(&json!({"schematic_scope": "board"})).unwrap_err();
    assert!(unknown.is_error);
    assert!(format!("{:?}", unknown.content).contains("expected 'file' or 'hierarchy'"));
    let wrong_type = requested_schematic_scope(&json!({"schematic_scope": 3})).unwrap_err();
    assert!(format!("{:?}", wrong_type.content).contains("expected a string"));
}

#[test]
fn an_audit_result_must_be_a_successful_json_object() {
    let ok = audit_result_json(text_result(r#"{"findings":[]}"#, false)).unwrap();
    assert!(ok.is_object());
    let errored = audit_result_json(text_result(r#"{"findings":[]}"#, true)).unwrap_err();
    assert!(errored.to_string().contains("error result"), "{errored}");
    let array = audit_result_json(text_result("[1]", false)).unwrap_err();
    assert!(array.to_string().contains("not a JSON object"), "{array}");
    let empty = audit_result_json(result_with(vec![], false)).unwrap_err();
    assert!(empty.to_string().contains("no content"), "{empty}");
    let image = audit_result_json(result_with(
        vec![ToolContent::Image {
            data: String::new(),
            mime_type: "image/png".into(),
        }],
        false,
    ))
    .unwrap_err();
    assert!(image.to_string().contains("non-text"), "{image}");
    assert!(audit_result_json(text_result("not json", false)).is_err());
}

fn decoupling_sheet() -> String {
    let lib = |name: &str, pin_name: &str| {
        format!(
            "(symbol \"{name}\" (symbol \"{short}_1_1\" (pin power_in line (at 0 0 0) (length 2.54) (name \"{pin_name}\") (number \"1\"))))",
            short = name.split(':').nth(1).unwrap()
        )
    };
    let place = |lib_id: &str, reference: &str, x: f64| {
        format!(
            "(symbol (lib_id \"{lib_id}\") (at {x} 20 0) (unit 1) (uuid \"{reference}-u\") (property \"Reference\" \"{reference}\" (at {x} 20 0)) (property \"Value\" \"v\" (at {x} 20 0)))"
        )
    };
    format!(
        "(kicad_sch (version 20250610) (generator \"konnect\") (uuid \"11111111-1111-4111-8111-111111111111\") (paper \"A4\") (lib_symbols {} {} {}) {} {} {})",
        lib("Chip:MCU", "VCC"),
        lib("Device:R_Small", "VCC"),
        lib("Connector:Conn_01x01", "VCC"),
        place("Chip:MCU", "U1", 20.0),
        place("Device:R_Small", "R1", 60.0),
        place("Connector:Conn_01x01", "J1", 100.0),
    )
}

#[tokio::test]
async fn the_decoupling_audit_checks_ics_only_and_flags_an_undecoupled_power_pin() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("d.kicad_sch");
    std::fs::write(&path, decoupling_sheet()).unwrap();
    let result = handle_audit_decoupling_file(
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
    assert_eq!(
        body["total_power_pins"], 1,
        "resistor and connector are skipped"
    );
    assert_eq!(body["pass_count"], 0);
    let findings = body["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["component"], "U1");
    assert_eq!(findings[0]["severity"], "error");
}

#[tokio::test]
async fn decoupling_audit_excludes_each_passive_and_connector_family() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("families.kicad_sch");
    for family in [
        "Device:C_Small",
        "Device:L_Small",
        "Device:D_Small",
        "Connector:Jack_Audio",
        "Connector:Header_01x01",
    ] {
        let source = decoupling_sheet()
            .replace("Device:R_Small", family)
            .replace(
                "R_Small_1_1",
                &format!("{}_1_1", family.split(':').nth(1).unwrap()),
            );
        std::fs::write(&path, source).unwrap();
        let result = audit_result_json(
            handle_audit_decoupling_file(&json!({"schematic":path}), &test_ctx())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["total_power_pins"], 1, "{family}");
        assert_eq!(result["findings"].as_array().unwrap().len(), 1);
    }
}

fn reset_pin_sheet(pin_name: &str, label: bool) -> String {
    let label = if label {
        "(label \"RESET_NET\" (at 20 20 0))"
    } else {
        ""
    };
    format!(
        r#"(kicad_sch (version 20250610) (generator "konnect") (uuid "11111111-1111-4111-8111-111111111111") (paper "A4")
      (lib_symbols (symbol "Chip:Reset" (symbol "Reset_1_1" (pin input line (at 0 0 0) (length 2.54) (name "{pin_name}") (number "1")))))
      (symbol (lib_id "Chip:Reset") (at 20 20 0) (unit 1) (uuid "22222222-2222-4222-8222-222222222222") (property "Reference" "U1") (property "Value" "Chip")) {label})"#
    )
}

#[tokio::test]
async fn connection_audit_warns_for_named_reset_inputs_but_not_outputs_or_unnamed_nets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reset.kicad_sch");
    for (name, label, warnings) in [
        ("RESET", true, 1),
        ("NRST", true, 1),
        ("RST", true, 1),
        ("~{RST}", true, 1),
        ("~{RST}", false, 0),
        ("RESET_OUT", true, 0),
        ("RESET", false, 0),
    ] {
        std::fs::write(&path, reset_pin_sheet(name, label)).unwrap();
        let result = audit_result_json(
            handle_audit_connections_file(&json!({"schematic":path}), &test_ctx())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            result["findings"].as_array().unwrap().len(),
            warnings,
            "{name}, label={label}"
        );
    }
}

#[tokio::test]
async fn power_audit_recognizes_ground_aliases_before_demanding_decoupling() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("aliases.kicad_sch");
    for ground in ["GND", "VSS"] {
        let source = format!(
            r#"(kicad_sch (version 20250610) (generator "konnect") (uuid "11111111-1111-4111-8111-111111111111") (paper "A4")
            (label "VCC" (at 20 20 0)) (label "{ground}" (at 20 20 0)))"#
        );
        std::fs::write(&path, source).unwrap();
        let result = audit_result_json(
            handle_audit_power_rails_file(&json!({"schematic":path}), &test_ctx())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["power_nets"].as_array().unwrap().len(), 1);
        let errors = result["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|finding| finding["severity"] == "error")
            .count();
        assert_eq!(errors, 0, "{ground}");
    }
}

#[tokio::test]
async fn bom_tilde_values_are_missing_and_explicit_mpn_satisfies_ic_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bom-tilde.kicad_sch");
    let source = reset_pin_sheet("IN",false).replace("(property \"Value\" \"Chip\")","(property \"Value\" \"~\") (property \"Footprint\" \"~\") (property \"MPN\" \"part-number\")");
    std::fs::write(&path, source).unwrap();
    let result = audit_result_json(
        handle_check_bom_health_file(&json!({"schematic":path}), &test_ctx())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["missing_value"], 1);
    assert_eq!(result["missing_footprint"], 1);
    assert_eq!(result["missing_mpn"], 0);
}
