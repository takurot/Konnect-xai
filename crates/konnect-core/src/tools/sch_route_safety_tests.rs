use super::*;
use konnect_sexp::parser::parse_sexp;
use serde_json::json;

fn tree(children: &str) -> SexpNode {
    parse_sexp(&format!("(kicad_sch {children})")).unwrap()
}
fn route() -> Vec<Segment> {
    route_segments((0., 0.), (10., 10.))
}
fn endpoints() -> [Point; 2] {
    [(0., 0.), (10., 10.)]
}

#[test]
fn foreign_pin_on_l_route_refuses_and_reports_net() {
    let sheet = tree(
        r#"
      (lib_symbols (symbol "Test:P" (symbol "P_1_1"
        (pin passive line (at 0 0 0) (length 1) (name "P") (number "1")))))
      (symbol (lib_id "Test:P") (at 5 0 0) (unit 1) (uuid "capacitor") (property "Reference" "C1"))
      (label "FOREIGN" (at 5 0 0))"#,
    );
    let error = check_route(&sheet, &route(), &endpoints(), None, false).unwrap_err();
    assert!(serde_json::to_string(&error).unwrap().contains("FOREIGN"));
}

#[test]
fn foreign_wire_crossing_is_refused_conservatively() {
    let sheet =
        tree(r#"(wire (pts (xy 5 -5) (xy 5 5)) (uuid "foreign")) (label "FOREIGN" (at 5 -5 0))"#);
    assert!(check_route(&sheet, &route(), &endpoints(), None, false).is_err());
}

#[test]
fn foreign_label_on_route_is_refused() {
    let sheet = tree(r#"(global_label "FOREIGN" (at 10 5 0))"#);
    assert!(check_route(&sheet, &route(), &endpoints(), None, false).is_err());
}

#[test]
fn foreign_sheet_pin_on_route_is_refused() {
    let sheet = tree(r#"(sheet (at 1 1) (pin "OUT" output (at 10 5 0)))"#);
    assert!(check_route(&sheet, &route(), &endpoints(), None, false).is_err());
}

#[test]
fn named_endpoint_net_is_authorized_at_other_locations() {
    let sheet = tree(r#"(label "VCC" (at 0 0 0)) (label "VCC" (at 10 5 0))"#);
    assert!(check_route(&sheet, &route(), &endpoints(), None, false).is_ok());
}

#[test]
fn explicit_override_authorizes_foreign_net() {
    let sheet = tree(r#"(label "FOREIGN" (at 10 5 0))"#);
    assert!(check_route(&sheet, &route(), &endpoints(), None, true).is_ok());
}

#[test]
fn named_stub_net_is_authorized() {
    let sheet = tree(r#"(label "TARGET" (at 10 5 0))"#);
    assert!(check_route(&sheet, &route(), &endpoints(), Some("TARGET"), false).is_ok());
}

#[test]
fn override_requires_boolean() {
    assert!(!allow_join(&json!({})).unwrap());
    assert!(allow_join(&json!({"allow_join": true})).unwrap());
    for invalid in [json!("true"), json!(1), json!(null), json!([])] {
        assert!(allow_join(&json!({"allow_join": invalid})).is_err());
    }
}

#[test]
fn selecting_a_pin_on_an_undotted_foreign_wire_does_not_select_that_wire() {
    let sheet = tree(
        r#"
      (lib_symbols (symbol "Test:P" (symbol "P_1_1"
        (pin passive line (at 0 0 0) (length 0) (name "P") (number "1")))))
      (symbol (lib_id "Test:P") (at 0 0 0) (unit 1) (uuid "selected") (property "Reference" "R1"))
      (wire (pts (xy -5 0) (xy 5 0)) (uuid "foreign"))
      (label "FOREIGN" (at -5 0 0))"#,
    );
    let error = check_route(&sheet, &route(), &endpoints(), None, false).unwrap_err();
    assert!(serde_json::to_string(&error).unwrap().contains("FOREIGN"));
}

#[test]
fn selecting_a_pin_already_joined_by_a_dot_authorizes_its_actual_wire_net() {
    let sheet = tree(
        r#"
      (lib_symbols (symbol "Test:P" (symbol "P_1_1"
        (pin passive line (at 0 0 0) (length 0) (name "P") (number "1")))))
      (symbol (lib_id "Test:P") (at 0 0 0) (unit 1) (uuid "selected") (property "Reference" "R1"))
      (wire (pts (xy -5 0) (xy 5 0)) (uuid "joined"))
      (label "SELECTED" (at -5 0 0))
      (junction (at 0 0))"#,
    );
    assert!(check_route(&sheet, &route(), &endpoints(), None, false).is_ok());
}
