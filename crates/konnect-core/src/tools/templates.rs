//! `templates` toolset — Reference circuit library for validated subcircuit templates.
//!
//! Templates are JSON files stored in `~/.konnect/templates/` (user) and
//! shipped as embedded defaults. Claude retrieves a template and adapts it to the
//! user's project — this prevents hallucinating component values.

use super::sch_annotate::format_designator;
use super::sch_references::ReferenceInventory;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::{get_path, invalid_arg, opt_u32, require_str, ToolContext, ToolDef};
use konnect_sexp::{geometry::round6, read_consistent};
use serde_json::json;
use std::path::PathBuf;
use tracing::{debug, info, warn};

// ─── Embedded starter templates ──────────────────────────────────────────────

fn builtin_templates() -> Vec<serde_json::Value> {
    // Serialize the large STM32 map one row at a time to keep json! expansion shallow.
    let stm32_connections: Vec<_> = [
        ("U1.1", "VCC_3V3"),
        ("U1.9", "VCC_3V3"),
        ("U1.24", "VCC_3V3"),
        ("U1.36", "VCC_3V3"),
        ("U1.48", "VCC_3V3"),
        ("J1.1", "VCC_3V3"),
        ("C3.1", "VCC_3V3"),
        ("C4.1", "VCC_3V3"),
        ("C5.1", "VCC_3V3"),
        ("C6.1", "VCC_3V3"),
        ("C7.1", "VCC_3V3"),
        ("R1.1", "VCC_3V3"),
        ("U1.8", "GND"),
        ("U1.23", "GND"),
        ("U1.35", "GND"),
        ("U1.47", "GND"),
        ("U1.49", "GND"),
        ("J1.3", "GND"),
        ("J1.5", "GND"),
        ("J1.9", "GND"),
        ("C1.2", "GND"),
        ("C2.2", "GND"),
        ("C3.2", "GND"),
        ("C4.2", "GND"),
        ("C5.2", "GND"),
        ("C6.2", "GND"),
        ("C7.2", "GND"),
        ("C8.2", "GND"),
        ("C9.2", "GND"),
        ("R2.2", "GND"),
        ("R3.2", "GND"),
        ("U1.5", "HSE_IN"),
        ("Y1.1", "HSE_IN"),
        ("C1.1", "HSE_IN"),
        ("U1.6", "HSE_OUT"),
        ("Y1.2", "HSE_OUT"),
        ("C2.1", "HSE_OUT"),
        ("U1.22", "VCAP"),
        ("C8.1", "VCAP"),
        ("U1.7", "NRST"),
        ("J1.10", "NRST"),
        ("R1.2", "NRST"),
        ("C9.1", "NRST"),
        ("U1.34", "SWDIO"),
        ("J1.2", "SWDIO"),
        ("U1.37", "SWCLK"),
        ("J1.4", "SWCLK"),
        ("U1.39", "SWO"),
        ("J1.6", "SWO"),
        ("U1.44", "BOOT0"),
        ("R2.1", "BOOT0"),
        ("U1.20", "BOOT1"),
        ("R3.1", "BOOT1"),
    ]
    .into_iter()
    .map(|(pin, net)| json!({"from": pin, "to_net": net}))
    .collect();

    vec![
        json!({
            "id": "usb_c_5v_sink",
            "name": "USB-C Power Sink (5V default)",
            "description": "USB Type-C receptacle with CC resistors for 5V default power and ESD protection on D+/D-.",
            "category": "connectivity/usb",
            "tags": ["usb-c", "power", "5v", "sink"],
            "components": [
                {"ref_prefix": "J", "lib_id": "Connector:USB_C_Receptacle_USB2.0_16P", "value": "USB_C", "notes": "USB 2.0 16-pin receptacle"},
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "5.1k", "quantity": 2, "package": "0402", "notes": "CC1 and CC2 pull-down — required for 5V default current"},
                {"ref_prefix": "D", "lib_id": "Power_Protection:PRTR5V0U2X", "value": "PRTR5V0U2X", "quantity": 1, "notes": "ESD protection on D+/D-"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "100nF", "quantity": 1, "package": "0402", "notes": "VBUS decoupling"}
            ],
            "connections": [
                {"from": "J1.A4", "to_net": "VUSB"},
                {"from": "J1.A9", "to_net": "VUSB"},
                {"from": "J1.B4", "to_net": "VUSB"},
                {"from": "J1.B9", "to_net": "VUSB"},
                {"from": "D1.4", "to_net": "VUSB"},
                {"from": "C1.1", "to_net": "VUSB"},
                {"from": "J1.A1", "to_net": "GND"},
                {"from": "J1.A12", "to_net": "GND"},
                {"from": "J1.B1", "to_net": "GND"},
                {"from": "J1.B12", "to_net": "GND"},
                {"from": "R1.2", "to_net": "GND"},
                {"from": "R2.2", "to_net": "GND"},
                {"from": "D1.1", "to_net": "GND"},
                {"from": "C1.2", "to_net": "GND"},
                {"from": "J1.A5", "to_net": "CC1"},
                {"from": "R1.1", "to_net": "CC1"},
                {"from": "J1.B5", "to_net": "CC2"},
                {"from": "R2.1", "to_net": "CC2"},
                {"from": "J1.A6", "to_net": "USB_DP"},
                {"from": "J1.B6", "to_net": "USB_DP"},
                {"from": "D1.2", "to_net": "USB_DP"},
                {"from": "J1.A7", "to_net": "USB_DN"},
                {"from": "J1.B7", "to_net": "USB_DN"},
                {"from": "D1.3", "to_net": "USB_DN"},
                {"from": "J1.SH", "to_net": "USB_SHIELD"}
            ],
            "design_notes": "Use one 5.1k pull-down per CC pin and follow the source's advertised current. SBU pins A8 and B8 are unused. Terminate USB_SHIELD according to the enclosure and EMC design.",
            "references": ["USB Type-C Spec Rev 2.0, Table 4-25"]
        }),
        json!({
            "id": "ldo_3v3",
            "name": "3.3V LDO Regulator",
            "description": "AP2112K-3.3 regulator with input and output ceramic capacitors.",
            "category": "power/regulator",
            "tags": ["ldo", "3v3", "regulator", "power"],
            "components": [
                {"ref_prefix": "U", "lib_id": "Regulator_Linear:AP2112K-3.3", "value": "AP2112K-3.3", "notes": "3.3V 600mA LDO"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "1uF", "quantity": 1, "package": "0603", "notes": "Input ceramic capacitor, X5R or X7R"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "1uF", "quantity": 1, "package": "0603", "notes": "Output ceramic capacitor, X5R or X7R"}
            ],
            "connections": [
                {"from": "U1.1", "to_net": "VIN"},
                {"from": "U1.3", "to_net": "VIN"},
                {"from": "C1.1", "to_net": "VIN"},
                {"from": "U1.2", "to_net": "GND"},
                {"from": "C1.2", "to_net": "GND"},
                {"from": "C2.2", "to_net": "GND"},
                {"from": "U1.5", "to_net": "VCC_3V3"},
                {"from": "C2.1", "to_net": "VCC_3V3"}
            ],
            "design_notes": "Place the 1uF input and output ceramic capacitors near the regulator. EN is tied to VIN for always-on operation; pin 4 is NC. Verify effective capacitance, dropout headroom and thermal limits for the load.",
            "references": ["Diodes Incorporated AP2112 datasheet DS39724"]
        }),
        json!({
            "id": "stm32_minimal",
            "name": "STM32 Minimal System",
            "description": "STM32 MCU with HSE crystal, decoupling caps, reset circuit, and SWD debug header.",
            "category": "mcu/stm32",
            "tags": ["stm32", "mcu", "minimal", "crystal", "swd"],
            "components": [
                {"ref_prefix": "U", "lib_id": "MCU_ST_STM32F4:STM32F411CEUx", "value": "STM32F411CEU6", "notes": "48-pin UFQFPN STM32F411; re-check the pinout before substituting"},
                {"ref_prefix": "Y", "lib_id": "Device:Crystal", "value": "8MHz", "quantity": 1, "notes": "HSE crystal — check MCU datasheet for supported range"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "20pF", "quantity": 2, "package": "0402", "notes": "Crystal load caps — calculate from datasheet CL spec"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "100nF", "quantity": 4, "package": "0402", "notes": "Decoupling caps — one per VDD/VDDA pin"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "4.7uF", "quantity": 1, "package": "0402", "notes": "Bulk decoupling on VDD"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "4.7uF", "quantity": 1, "notes": "VCAP1 capacitor, ESR below 1 ohm"},
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "10k", "quantity": 1, "package": "0402", "notes": "NRST pull-up"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "100nF", "quantity": 1, "package": "0402", "notes": "NRST filter cap to GND"},
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "10k", "quantity": 2, "package": "0402", "notes": "BOOT0 and PB2/BOOT1 pull-downs"},
                {"ref_prefix": "J", "lib_id": "Connector:Conn_ARM_JTAG_SWD_10", "value": "SWD", "notes": "10-pin ARM SWD debug header"}
            ],
            "connections": stm32_connections,
            "design_notes": "Size the HSE load capacitors for the selected crystal. ST estimates 10pF combined pin and board capacitance for sizing. Put the four 100nF caps near the three VDD pins and VDDA. VBAT ties to VDD in this circuit without a backup battery. Use 4.7uF with ESR below 1 ohm on VCAP1. Leave SWD header key pin 7 and JTAG-only pin 8 unused.",
            "references": ["AN4488: Getting started with STM32F4 MCU hardware development"]
        }),
        json!({
            "id": "i2c_pullups",
            "name": "I2C Bus Pull-ups",
            "description": "Standard I2C pull-up resistors for SDA and SCL lines.",
            "category": "connectivity/i2c",
            "tags": ["i2c", "pull-up", "bus"],
            "components": [
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "4.7k", "quantity": 2, "package": "0402", "notes": "SDA and SCL pull-ups. Use 2.2k for fast-mode (400kHz), 1k for fast-mode plus (1MHz)"}
            ],
            "connections": [
                {"from": "R1.1", "to_net": "SDA", "notes": "I2C data line"},
                {"from": "R1.2", "to_net": "VCC_3V3", "notes": "Pull to I2C bus voltage"},
                {"from": "R2.1", "to_net": "SCL", "notes": "I2C clock line"},
                {"from": "R2.2", "to_net": "VCC_3V3", "notes": "Pull to I2C bus voltage"}
            ],
            "design_notes": "One set of pull-ups per I2C bus — do NOT add pull-ups on every device. Value depends on bus speed and capacitance. 4.7k is safe for standard mode (100kHz) with <400pF bus capacitance.",
            "references": ["NXP UM10204: I2C-bus specification"]
        }),
        json!({
            "id": "led_indicator",
            "name": "LED Indicator Circuit",
            "description": "Simple LED with current-limiting resistor, driven by a GPIO pin.",
            "category": "misc/led",
            "tags": ["led", "indicator", "gpio"],
            "components": [
                {"ref_prefix": "D", "lib_id": "Device:LED", "value": "LED_Green", "quantity": 1, "package": "0603", "notes": "Standard indicator LED"},
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "1k", "quantity": 1, "package": "0402", "notes": "Current limiter: R = (Vcc - Vf) / If. For 3.3V, green Vf≈2.1V, If=1.2mA → 1k"}
            ],
            "connections": [
                {"from": "R1.1", "to_net": "GPIO"},
                {"from": "R1.2", "to_net": "LED_A"},
                {"from": "D1.2", "to_net": "LED_A"},
                {"from": "D1.1", "to_net": "GND"}
            ],
            "design_notes": "R = (VCC - Vf) / If. For 3.3V GPIO: green (Vf=2.1V) → 1k gives 1.2mA. Red (Vf=1.8V) → 680R gives 2.2mA. Bright LEDs may only need 0.5mA. Check your LED's datasheet for Vf and recommended If.",
            "references": []
        }),
        json!({
            "id": "buck_converter",
            "name": "Buck Converter (Step-Down)",
            "description": "Synchronous buck converter with input/output caps, inductor, and feedback resistors. Generic topology — adapt MPN and passives to your voltage/current needs.",
            "category": "power/switching",
            "tags": ["buck", "step-down", "switching", "regulator", "power"],
            "components": [
                {"ref_prefix": "U", "lib_id": "Regulator_Switching:TPS563200", "value": "TPS563200DDCR", "notes": "3A sync buck, 4.5-17V input. Substitute: AP63356, MP2315, SY8089"},
                {"ref_prefix": "L", "lib_id": "Device:L", "value": "4.7uH", "quantity": 1, "notes": "Inductor — check datasheet for recommended value and saturation current > Iout*1.3"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "10uF", "quantity": 2, "package": "0805", "notes": "Input capacitors — X5R/X7R ceramic, voltage rating > Vin*1.5"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "22uF", "quantity": 2, "package": "0805", "notes": "Output capacitors — low ESR ceramic"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "100nF", "quantity": 1, "package": "0402", "notes": "Boot cap"},
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "73.2k", "quantity": 1, "package": "0402", "notes": "Feedback upper resistor — adjust for target Vout"},
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "22.1k", "quantity": 1, "package": "0402", "notes": "Feedback lower resistor — Vout = Vref * (1 + Rtop/Rbot)"}
            ],
            "connections": [
                {"from": "U1.3", "to_net": "VIN"},
                {"from": "U1.5", "to_net": "VIN"},
                {"from": "C1.1", "to_net": "VIN"},
                {"from": "C2.1", "to_net": "VIN"},
                {"from": "U1.1", "to_net": "GND"},
                {"from": "C1.2", "to_net": "GND"},
                {"from": "C2.2", "to_net": "GND"},
                {"from": "C3.2", "to_net": "GND"},
                {"from": "C4.2", "to_net": "GND"},
                {"from": "R2.2", "to_net": "GND"},
                {"from": "U1.2", "to_net": "SW"},
                {"from": "L1.1", "to_net": "SW"},
                {"from": "C5.2", "to_net": "SW"},
                {"from": "L1.2", "to_net": "VOUT"},
                {"from": "C3.1", "to_net": "VOUT"},
                {"from": "C4.1", "to_net": "VOUT"},
                {"from": "R1.1", "to_net": "VOUT"},
                {"from": "U1.4", "to_net": "FB"},
                {"from": "R1.2", "to_net": "FB"},
                {"from": "R2.1", "to_net": "FB"},
                {"from": "U1.6", "to_net": "BOOT"},
                {"from": "C5.1", "to_net": "BOOT"}
            ],
            "design_notes": "The divider gives approximately 3.3V at the 0.765V feedback reference. Recalculate the divider, inductor and capacitors for other output targets. Place the input capacitors by VIN/GND and the divider by VFB away from the SW node.",
            "references": ["TI TPS563200 datasheet SLVSCB0E"]
        }),
    ]
}

// ─── Template storage paths ──────────────────────────────────────────────────

fn user_templates_dir() -> Option<PathBuf> {
    use konnect_sexp::state::{self, StateDir};

    let state = StateDir::resolve().ok()?;
    Some(state.existing_or_default("templates", || {
        #[cfg(windows)]
        let old = state::legacy_roaming_dir();
        #[cfg(not(windows))]
        let old = state::legacy_home_dir();
        old.map(|dir| dir.join("templates"))
    }))
}

/// Load all templates: builtins + any user-created ones from disk.
async fn load_all_templates() -> Vec<serde_json::Value> {
    let mut templates = builtin_templates();
    templates.extend(load_user_templates().await);
    templates
}

async fn load_user_templates() -> Vec<serde_json::Value> {
    let mut templates = Vec::new();
    if let Some(user_dir) = user_templates_dir() {
        if let Ok(mut rd) = tokio::fs::read_dir(&user_dir).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    match tokio::fs::read_to_string(&path).await {
                        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                            Ok(tmpl) => {
                                debug!(path = %path.display(), "Loaded user template");
                                templates.push(tmpl);
                            }
                            Err(e) => {
                                warn!(path = %path.display(), error = %e, "Failed to parse user template")
                            }
                        },
                        Err(e) => {
                            warn!(path = %path.display(), error = %e, "Failed to read user template")
                        }
                    }
                }
            }
        }
    }

    templates
}

// ─── Tool definitions ─────────────────────────────────────────────────────────

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "search_templates",
            "Search the reference circuit template library. Returns matching templates for \
             common subcircuits (USB-C, LDO, buck converter, MCU minimal system, I2C pull-ups, etc.). \
             Use these instead of designing from scratch — templates have verified component values.",
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search string: 'usb-c', '3.3v regulator', 'stm32', 'i2c', 'led', 'buck converter', etc."
                    },
                    "category": {
                        "type": "string",
                        "description": "Filter by category: 'power', 'connectivity', 'mcu', 'misc' (optional)"
                    }
                },
                "required": ["query"]
            }),
            |args, ctx| async move { handle_search_templates(args, ctx).await }
        ),
        tool!(
            "get_template",
            "Get full details for a reference circuit template including all components, \
             connections, and design notes. Use the template ID from search_templates.",
            json!({
                "type": "object",
                "properties": {
                    "template_id": { "type": "string", "description": "Template ID (e.g. 'usb_c_5v_sink', 'ldo_3v3', 'stm32_minimal')" }
                },
                "required": ["template_id"]
            }),
            |args, ctx| async move { handle_get_template(args, ctx).await }
        ),
        tool!(
            "apply_template",
            "Apply a reference circuit template. This places components in the schematic and \
             returns a connection guide. It does not wire pins or add labels. Use net_mappings \
             to rename nets in the guide.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "template_id": { "type": "string", "description": "Template ID to instantiate" },
                    "position_x": { "type": "number", "description": "X position to place the subcircuit (mm)", "default": 100.0 },
                    "position_y": { "type": "number", "description": "Y position to place the subcircuit (mm)", "default": 100.0 },
                    "net_mappings": {
                        "type": "object",
                        "additionalProperties": true,
                        "description": "Rename nets in the returned guide. E.g. {\"VUSB\": \"VCC_5V\", \"GND\": \"GND\"}"
                    },
                    "ref_start": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "First reference number to consider. Occupied numbers are skipped. Auto-detected if omitted."
                    }
                },
                "required": ["schematic", "template_id"]
            }),
            |args, ctx| async move { handle_apply_template(args, ctx).await }
        )
        .with_kicad_config(),
        tool!(
            "list_template_categories",
            "List all available template categories and the number of templates in each.",
            json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            |args, ctx| async move { handle_list_categories(args, ctx).await }
        ),
    ]
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn handle_search_templates(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let query = match require_str(args, "query") {
        Ok(v) => v.to_lowercase(),
        Err(e) => return Ok(e),
    };
    let category_filter = args["category"].as_str().map(|s| s.to_lowercase());

    info!(query = %query, category = ?category_filter, "Searching templates");

    let templates = load_all_templates().await;
    let mut results = Vec::new();

    for tmpl in &templates {
        let id = tmpl["id"].as_str().unwrap_or("");
        let name = tmpl["name"].as_str().unwrap_or("");
        let desc = tmpl["description"].as_str().unwrap_or("");
        let category = tmpl["category"].as_str().unwrap_or("");
        let tags: Vec<&str> = tmpl["tags"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        // Category filter
        if let Some(ref cat) = category_filter {
            if !category.to_lowercase().contains(cat) {
                continue;
            }
        }

        // Search across name, description, tags, category
        let haystack =
            format!("{} {} {} {} {}", id, name, desc, category, tags.join(" ")).to_lowercase();

        let matches = query.split_whitespace().all(|word| haystack.contains(word));

        if matches {
            let component_count: usize =
                tmpl["components"].as_array().map(|a| a.len()).unwrap_or(0);
            results.push(json!({
                "id": id,
                "name": name,
                "description": desc,
                "category": category,
                "tags": tags,
                "component_count": component_count
            }));
        }
    }

    debug!(query = %query, results = results.len(), "Template search complete");

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "query": query,
            "count": results.len(),
            "templates": results
        }))
        .unwrap(),
    ))
}

async fn handle_get_template(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let template_id = match require_str(args, "template_id") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };

    info!(template_id = %template_id, "Loading template");

    let templates = load_all_templates().await;
    let tmpl = templates
        .iter()
        .find(|t| t["id"].as_str() == Some(&template_id));

    match tmpl {
        Some(t) => Ok(CallToolResult::text(serde_json::to_string(t).unwrap())),
        None => {
            warn!(template_id = %template_id, "Template not found");
            Ok(CallToolResult::error(format!(
                "Template '{}' not found. Use search_templates to find available templates.",
                template_id
            )))
        }
    }
}

async fn handle_apply_template(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let template_id = match require_str(args, "template_id") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let base_x = args["position_x"].as_f64().unwrap_or(100.0);
    let base_y = args["position_y"].as_f64().unwrap_or(100.0);
    let net_mappings: std::collections::HashMap<String, String> = args["net_mappings"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    info!(
        template_id = %template_id,
        schematic = %sch_path.display(),
        position = ?(base_x, base_y),
        net_mappings = ?net_mappings,
        "Applying template"
    );

    let find_template =
        |template: &serde_json::Value| template["id"].as_str() == Some(&template_id);
    let builtin = builtin_templates().into_iter().find(find_template);
    let is_builtin = builtin.is_some();
    let template = match builtin {
        Some(template) => Some(template),
        None => load_user_templates().await.into_iter().find(find_template),
    };
    let tmpl = match template {
        Some(template) => template,
        None => {
            warn!(template_id = %template_id, "Template not found for apply");
            return Ok(CallToolResult::error(format!(
                "Template '{}' not found",
                template_id
            )));
        }
    };

    // Templates are ordinary symbol placements. Load and validate the same
    // instance context used by the single and batch placement tools rather
    // than hand-writing a second, incomplete interpretation of KiCad's
    // instance metadata (#609).
    let before = match read_consistent(&sch_path) {
        Ok(source) => source,
        Err(error) => return Ok(CallToolResult::error(error.to_string())),
    };
    let mut sch = match konnect_schematic_editor::Schematic::from_source(&sch_path, before.as_str())
    {
        Ok(schematic) => schematic,
        Err(error) => return Ok(CallToolResult::error(error.to_string())),
    };
    let context = match crate::tools::sheet_instance_context(&sch_path, &mut sch) {
        Ok(context) => context,
        Err(error) => return Ok(error.into_tool_result()),
    };
    if let Err(error) = crate::tools::validate_sheet_instance_state(&sch_path, &sch, &context) {
        return Ok(error.into_tool_result());
    }
    let source = match crate::tools::library::KiCadSymbolSource::for_file(&sch_path) {
        Ok(source) => source,
        Err(error) => return Ok(error.into_tool_result()),
    };

    let references =
        match ReferenceInventory::load(&sch_path, &before, &sch.to_source(), Some(&context)) {
            Ok(inventory) => inventory,
            Err(error) => return Ok(error),
        };
    let ref_start = match opt_u32(args, "ref_start") {
        Ok(Some(0)) => return Ok(invalid_arg("ref_start", "expected a positive integer")),
        Ok(start) => start.unwrap_or_else(|| references.template_start()),
        Err(error) => return Ok(error),
    };

    let components = tmpl["components"].as_array().cloned().unwrap_or_default();
    let mut placed = Vec::new();
    let mut placed_targets = Vec::new();
    let mut placed_by_template_ref = std::collections::HashMap::new();
    let mut ref_counters = std::collections::HashMap::<String, Option<u32>>::new();
    let mut template_counters = std::collections::HashMap::<String, usize>::new();

    // Place components in a column layout
    let spacing_y = 15.0; // mm between components
    for comp in components.iter() {
        let ref_prefix = comp["ref_prefix"].as_str().unwrap_or("U");
        let lib_id = comp["lib_id"].as_str().unwrap_or("Device:R");
        let value = comp["value"].as_str().unwrap_or("");
        let quantity = comp["quantity"].as_u64().unwrap_or(1) as usize;
        let notes = comp["notes"].as_str().unwrap_or("");

        for _q in 0..quantity {
            let counter = ref_counters
                .entry(ref_prefix.to_string())
                .or_insert(Some(ref_start));
            let Some(number) = counter.and_then(|first| references.next_unused(ref_prefix, first))
            else {
                return Ok(CallToolResult::error(format!(
                    "No free '{ref_prefix}' reference number"
                )));
            };
            *counter = number.checked_add(1);
            let reference = format_designator(ref_prefix, number);
            if is_builtin {
                let template_number = template_counters.entry(ref_prefix.to_string()).or_default();
                *template_number += 1;
                let template_ref = format!("{}{}", ref_prefix, template_number);
                placed_by_template_ref.insert(template_ref, reference.clone());
            }

            let x = base_x;
            let y = base_y + (placed.len() as f64) * spacing_y;
            let placement = match crate::tools::sch_components::place_one_component(
                &mut sch,
                &context.instance_paths,
                &context.project_name,
                lib_id,
                x,
                y,
                0.0,
                None,
                Some(&reference),
                Some(value),
                None,
                1,
                &source,
            ) {
                Ok(placement) => placement,
                Err(error) => return Ok(error),
            };
            placed_targets.push((placement.uuid, reference.clone()));

            placed.push(json!({
                "reference": reference,
                "lib_id": lib_id,
                "value": value,
                "x": round6(x), "y": round6(y),
                "notes": notes
            }));

            debug!(reference = %reference, lib_id = %lib_id, value = %value, "Placed template component");
        }
    }

    let candidate = sch.to_source();
    if let Err(error) = references.validate(&candidate) {
        return Ok(error);
    }
    if let Err(error) = references.commit(&before, &candidate) {
        return Ok(crate::tools::mutation_outcome_uncertain(
            &sch_path,
            "apply_template",
            format!("schematic persistence failed: {error}"),
        ));
    }
    let committed = match konnect_schematic_editor::Schematic::load(&sch_path) {
        Ok(schematic) => schematic,
        Err(error) => {
            return Ok(crate::tools::mutation_outcome_uncertain(
                &sch_path,
                "apply_template",
                format!("saved schematic could not be reloaded: {error}"),
            ))
        }
    };
    if let Err(error) = crate::tools::validate_sheet_instance_state(&sch_path, &committed, &context)
    {
        return Ok(crate::tools::mutation_outcome_uncertain(
            &sch_path,
            "apply_template",
            format!("saved template instance validation failed: {error}"),
        ));
    }
    for (uuid, reference) in &placed_targets {
        if !committed
            .symbols
            .as_slice()
            .iter()
            .any(|symbol| symbol.uuid == *uuid && symbol.reference() == Some(reference))
        {
            return Ok(crate::tools::mutation_outcome_uncertain(
                &sch_path,
                "apply_template",
                format!("saved template readback did not contain {reference} with UUID {uuid}"),
            ));
        }
    }

    info!(
        template_id = %template_id,
        components_placed = placed.len(),
        "Template applied successfully"
    );

    // Build the net mapping guide for the user/Claude to wire up
    let connections = tmpl["connections"].as_array().cloned().unwrap_or_default();
    let mapped_connections: Vec<serde_json::Value> = connections
        .iter()
        .map(|conn| {
            let mut c = conn.clone();
            for side in ["from", "to"] {
                let placed_pin = c[side]
                    .as_str()
                    .and_then(|endpoint| endpoint.split_once('.'))
                    .and_then(|(reference, pin)| {
                        placed_by_template_ref
                            .get(reference)
                            .map(|placed| format!("{placed}.{pin}"))
                    });
                if let Some(placed_pin) = placed_pin {
                    c[side] = json!(placed_pin);
                }
            }
            let original_net = c["to_net"].as_str().map(String::from);
            if let Some(net) = original_net {
                if let Some(mapped) = net_mappings.get(&net) {
                    c["to_net"] = json!(mapped);
                    c["mapped_from"] = json!(net);
                }
            }
            c
        })
        .collect();

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "template": template_id,
            "components_placed": placed,
            "wired": false,
            "connections_to_wire": mapped_connections,
            "design_notes": tmpl["design_notes"],
            "next_steps": "No wires or labels were added. Review the guide, then use connect_to_net or connect_pins to wire the placed components."
        }))
        .unwrap(),
    ))
}

async fn handle_list_categories(
    _args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let templates = load_all_templates().await;
    let mut categories: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for tmpl in &templates {
        let cat = tmpl["category"]
            .as_str()
            .unwrap_or("uncategorized")
            .to_string();
        *categories.entry(cat).or_insert(0) += 1;
    }

    let mut sorted: Vec<_> = categories.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "categories": sorted.iter().map(|(cat, count)| json!({"category": cat, "count": count})).collect::<Vec<_>>(),
            "total_templates": templates.len()
        }))
        .unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::ToolRouter;
    use crate::tools::ServerConfig;
    use std::sync::Arc;

    fn test_ctx() -> Arc<ToolContext> {
        Arc::new(ToolContext::new(
            ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            Arc::new(ToolRouter::new()),
        ))
    }

    fn template_fixture(test_name: &str) -> Option<std::path::PathBuf> {
        const FIXTURE_ROOT: &str = "KONNECT_TEST_TEMPLATE_ROOT";
        let root = match std::env::var_os(FIXTURE_ROOT) {
            Some(root) => std::path::PathBuf::from(root),
            None => {
                let dir = tempfile::tempdir().unwrap();
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", &format!("tools::templates::tests::{test_name}")])
                    .env(FIXTURE_ROOT, dir.path())
                    .env("KONNECT_STATE_DIR", dir.path().join("state"))
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return None;
            }
        };
        let template_dir = root.join("state/templates");
        std::fs::create_dir_all(&template_dir).unwrap();
        std::fs::write(
            template_dir.join("one_resistor.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "test_one_resistor",
                "name": "One resistor",
                "description": "Hermetic placement fixture",
                "category": "test",
                "components": [
                    {"ref_prefix": "R", "lib_id": "Device:R", "value": "4.7k"}
                ],
                "connections": [{"from": "R1.1", "to_net": "USER_NET"}],
                "design_notes": "test"
            }))
            .unwrap(),
        )
        .unwrap();

        // A real KiCad schematic fixture, including a fully embedded
        // Device:R definition, makes this independent of a KiCad installation.
        let schematic = root.join("derived_lib_name.kicad_sch");
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/derived_lib_name.kicad_sch"
            ),
            &schematic,
        )
        .unwrap();
        std::fs::write(root.join("derived_lib_name.kicad_pro"), "{}\n").unwrap();

        Some(schematic)
    }

    async fn call_registered(
        definitions: Vec<ToolDef>,
        name: &str,
        args: serde_json::Value,
        ctx: Arc<ToolContext>,
    ) -> CallToolResult {
        let tool = definitions
            .into_iter()
            .find(|tool| tool.name == name)
            .expect("registered tool");
        (tool.handler)(&args, ctx).await.unwrap()
    }

    #[tokio::test]
    async fn apply_template_uses_project_identity_and_leaves_batch_writes_usable() {
        let Some(schematic) =
            template_fixture("apply_template_uses_project_identity_and_leaves_batch_writes_usable")
        else {
            return;
        };
        let ctx = test_ctx();

        let applied = call_registered(
            tools(),
            "apply_template",
            json!({
                "schematic": schematic.display().to_string(),
                "template_id": "test_one_resistor",
                "net_mappings": { "USER_NET": "PROJECT_NET" },
                "position_x": 180.0,
                "position_y": 100.0
            }),
            ctx.clone(),
        )
        .await;
        assert!(!applied.is_error, "{applied:?}");

        let crate::mcp::protocol::ToolContent::Text { text } = &applied.content[0] else {
            panic!("expected template placement result");
        };
        let result: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            result["connections_to_wire"],
            json!([{"from": "R1.1", "to_net": "PROJECT_NET", "mapped_from": "USER_NET"}]),
            "user-defined endpoints keep their original meaning"
        );

        let saved = konnect_schematic_editor::Schematic::load(&schematic).unwrap();
        let placed = saved
            .symbols
            .as_slice()
            .iter()
            .find(|symbol| symbol.position().0 > 175.0)
            .expect("template-created symbol");
        let instances = placed.instances();
        assert_eq!(instances.len(), 1, "{instances:?}");
        assert_eq!(instances[0].project.as_deref(), Some("derived_lib_name"));
        assert_eq!(
            instances[0].path.as_deref(),
            Some("/11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(instances[0].reference.as_deref(), Some("R3"));
        assert_eq!(instances[0].unit, Some(1));
        let source = std::fs::read_to_string(&schematic).unwrap();
        assert!(!source.contains("(project \"\""), "{source}");

        // Exercise the registered batch tool after the template mutation. The
        // original defect made this exact next call fail stale_target.
        let batch = call_registered(
            crate::tools::sch_batch::tools(),
            "batch_place_components",
            json!({
                "schematic": schematic.display().to_string(),
                "components": [{
                    "lib_id": "Device:R", "reference": "R99",
                    "x": 190.0, "y": 100.0
                }]
            }),
            ctx,
        )
        .await;
        assert!(!batch.is_error, "{batch:?}");
        assert!(konnect_schematic_editor::Schematic::load(&schematic)
            .unwrap()
            .symbols
            .by_reference("R99")
            .is_some());
    }

    #[tokio::test]
    async fn repeated_template_skips_occupied_numbers_and_preserves_guide_ids() {
        let Some(schematic) =
            template_fixture("repeated_template_skips_occupied_numbers_and_preserves_guide_ids")
        else {
            return;
        };
        let ctx = test_ctx();
        let arguments = json!({
            "schematic": schematic,
            "template_id": "test_one_resistor",
            "ref_start": 1,
            "position_x": 180.0,
            "position_y": 100.0
        });
        for expected in ["R3", "R4"] {
            let result =
                call_registered(tools(), "apply_template", arguments.clone(), ctx.clone()).await;
            assert!(!result.is_error, "{result:?}");
            let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
                panic!("expected template placement result");
            };
            let response: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(response["connections_to_wire"][0]["from"], "R1.1");
            let saved = konnect_schematic_editor::Schematic::load(&schematic).unwrap();
            assert_eq!(
                saved
                    .symbols
                    .iter()
                    .filter(|symbol| symbol.reference() == Some(expected))
                    .count(),
                1
            );
        }
        let before = std::fs::read(&schematic).unwrap();
        let mut invalid = arguments;
        invalid["ref_start"] = json!(-1);
        let refused = call_registered(tools(), "apply_template", invalid, ctx).await;
        assert!(refused.is_error, "{refused:?}");
        assert_eq!(std::fs::read(&schematic).unwrap(), before);
    }

    #[tokio::test]
    async fn apply_template_refuses_malformed_instance_metadata_without_writing() {
        let Some(schematic) =
            template_fixture("apply_template_refuses_malformed_instance_metadata_without_writing")
        else {
            return;
        };
        let malformed = std::fs::read_to_string(&schematic).unwrap().replacen(
            "(project \"derived_lib_name\"",
            "(project \"\"",
            1,
        );
        assert_ne!(
            malformed,
            std::fs::read_to_string(&schematic).unwrap(),
            "fixture must contain an instance project to corrupt"
        );
        std::fs::write(&schematic, &malformed).unwrap();
        let before = std::fs::read(&schematic).unwrap();

        let refused = call_registered(
            tools(),
            "apply_template",
            json!({
                "schematic": schematic.display().to_string(),
                "template_id": "test_one_resistor"
            }),
            test_ctx(),
        )
        .await;
        assert!(refused.is_error, "{refused:?}");
        assert_eq!(
            crate::mcp::error::extract_error_kind(&refused).as_deref(),
            Some("stale_target")
        );
        assert_eq!(std::fs::read(&schematic).unwrap(), before);
    }

    #[tokio::test]
    async fn apply_template_snaps_and_warns_about_a_placement_off_the_page() {
        let Some(schematic) =
            template_fixture("apply_template_snaps_and_warns_about_a_placement_off_the_page")
        else {
            return;
        };
        let applied = call_registered(
            tools(),
            "apply_template",
            json!({
                "schematic": schematic.display().to_string(),
                "template_id": "test_one_resistor",
                "position_x": 100.33,
                "position_y": -7.1
            }),
            test_ctx(),
        )
        .await;
        assert!(!applied.is_error, "{applied:?}");
        let crate::mcp::protocol::ToolContent::Text { text } = &applied.content[0] else {
            panic!("expected template placement result");
        };
        let result: serde_json::Value = serde_json::from_str(text).unwrap();
        let placed = &result["components_placed"][0];
        assert_eq!(placed["x"], json!(100.33), "{result}");
        assert_eq!(placed["y"], json!(-7.62), "{result}");
        let warnings = result["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1, "{result}");
        assert!(
            warnings[0]
                .as_str()
                .unwrap()
                .contains("outside the 297 x 210 mm page"),
            "{result}"
        );
    }
}
