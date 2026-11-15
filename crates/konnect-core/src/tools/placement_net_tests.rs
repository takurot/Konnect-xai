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
