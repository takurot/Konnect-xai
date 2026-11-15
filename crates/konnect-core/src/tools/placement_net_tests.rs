use super::*;

#[test]
fn unconnected_and_empty_nets_pull_nothing() {
    for net in ["", "NC", "nc", "unconnected-(U1-Pad3)", "UNCONNECTED"] {
        assert_eq!(net_weight(net), 0.0, "{net:?} must not attract parts");
    }
}

#[test]
fn differential_pairs_pull_hardest() {
    for net in ["USB_P", "usb_n", "CLK_P", "LVDS_N"] {
        assert_eq!(net_weight(net), 5.0, "{net}");
    }
}

#[test]
fn power_and_ground_rails_pull_moderately() {
    for net in [
        "GND", "gnda", "GNDD", "VCC", "VDD", "VSS", "+3V3", "-5V", "+12V",
    ] {
        assert_eq!(net_weight(net), 3.0, "{net}");
    }
}

#[test]
fn ordinary_signals_pull_with_unit_weight() {
    for net in ["SDA", "Net-(R1-Pad1)", "VCC_SENSE", "GND_SENSE"] {
        assert_eq!(net_weight(net), 1.0, "{net}");
    }
}

fn refs_on(nets: &[&'static str]) -> BTreeSet<&'static str> {
    nets.iter().copied().collect()
}

#[test]
fn an_unproven_decoupling_pairing_says_why_it_was_unproven() {
    let bbox: Bbox = (0.0, 0.0, 1.0, 1.0);
    let candidates = [("U1", bbox)];
    let u1_nets: HashMap<&str, BTreeSet<&str>> = HashMap::from([("U1", refs_on(&["RAIL"]))]);
    let wide: BTreeSet<&str> = (0..=MAX_DECOUPLING_NET_REFERENCES)
        .map(|i| Box::leak(format!("R{i}").into_boxed_str()) as &str)
        .collect();
    let narrow: BTreeSet<&str> = refs_on(&["U1", "C1"]);

    let reason = |cap: Option<BTreeSet<&str>>, by_net: HashMap<&str, BTreeSet<&str>>| {
        unproven_decoupling_reason(cap.as_ref(), &candidates, &u1_nets, &by_net)
    };

    assert!(reason(None, HashMap::new()).contains("no named pad nets"));
    assert!(reason(Some(refs_on(&["GND", "VSS"])), HashMap::new()).contains("only ground-like"));
    assert!(reason(
        Some(refs_on(&["GND", "RAIL"])),
        HashMap::from([("RAIL", wide)])
    )
    .contains("fanout limit"));
    assert!(reason(
        Some(refs_on(&["OTHER"])),
        HashMap::from([("OTHER", narrow.clone())])
    )
    .contains("no IC shares"));
    assert!(
        reason(Some(refs_on(&["RAIL"])), HashMap::from([("RAIL", narrow)]))
            .contains("could be proved")
    );
}

#[test]
fn ground_like_nets_are_recognised_across_naming_styles() {
    for net in [
        "GND",
        "/GND",
        " gnd ",
        "AGND",
        "Earth_GND",
        "DGND",
        "GND-ISO",
        "VSS",
        "VSSA",
        "0",
        "0V",
        "/0V",
        "G-N-D",
    ] {
        assert!(is_ground_like_net(net), "{net:?} is ground");
    }
    for net in ["VCC", "3V3", "SDA", "", "10", "AGNDX"] {
        assert!(!is_ground_like_net(net), "{net:?} is not ground");
    }
}

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../konnect-sexp/tests/fixtures/placement/placement_fixture.kicad_pcb"
);

fn ctx() -> ToolContext {
    ToolContext::new(
        crate::tools::ServerConfig::default(),
        std::sync::Arc::new(crate::router::ToolRouter::new()),
    )
}

fn message(result: &CallToolResult) -> String {
    format!("{:?}", result.content)
}

#[tokio::test]
async fn decoupling_placement_refuses_an_unknown_side_and_names_the_choices() {
    let result = handle_place_decoupling(
        &json!({"board": FIXTURE, "ic_reference": "U1", "capacitor_references": ["C1"], "side": "diagonal"}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(result.is_error, "{}", message(&result));
    assert!(
        message(&result).contains("auto, left, right, top, bottom"),
        "{}",
        message(&result)
    );
}

#[tokio::test]
async fn decoupling_placement_plans_a_row_on_the_requested_side_without_writing() {
    let before = std::fs::read_to_string(FIXTURE).unwrap();
    let result = handle_place_decoupling(
        &json!({"board": FIXTURE, "ic_reference": "U1", "capacitor_references": ["C1", "C2"], "side": "left"}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{}", message(&result));
    let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text");
    };
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["dry_run"], true, "dry run is the default: {body}");
    assert_eq!(std::fs::read_to_string(FIXTURE).unwrap(), before);
}

#[tokio::test]
async fn bga_fanout_refuses_an_unknown_strategy_and_a_footprint_without_pads() {
    let strategy = handle_bga_fanout(
        &json!({"board": FIXTURE, "reference": "U2", "strategy": "spiral"}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(strategy.is_error);
    assert!(
        message(&strategy).contains("dogbone or inline"),
        "{}",
        message(&strategy)
    );

    let absent = handle_bga_fanout(&json!({"board": FIXTURE, "reference": "Z99"}), &ctx())
        .await
        .unwrap();
    assert!(absent.is_error);
    assert!(message(&absent).contains("Z99"), "{}", message(&absent));
}
