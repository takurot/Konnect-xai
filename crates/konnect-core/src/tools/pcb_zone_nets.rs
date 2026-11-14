use anyhow::{bail, ensure, Context, Result};
use konnect_ipc::gen::kiapi::board::types::{zone::Settings, Net, Zone, ZoneType};
use prost::Message;

pub(super) fn routed_net(item: &prost_types::Any) -> Result<Option<Net>> {
    ensure!(
        konnect_ipc::builders::any_is(item, "kiapi.board.types.Zone"),
        "KiCad returned a zone item with unexpected type '{}'",
        item.type_url
    );
    let zone =
        Zone::decode(item.value.as_slice()).context("KiCad returned an invalid zone item")?;
    let identity = zone
        .id
        .as_ref()
        .map(|id| id.value.as_str())
        .filter(|id| !id.is_empty())
        .unwrap_or("<unidentified>");
    let net = match (ZoneType::try_from(zone.r#type), zone.settings) {
        (Ok(ZoneType::ZtRuleArea), Some(Settings::RuleAreaSettings(_)))
        | (Ok(ZoneType::ZtGraphical), None) => return Ok(None),
        (
            Ok(ZoneType::ZtCopper | ZoneType::ZtTeardrop),
            Some(Settings::CopperSettings(settings)),
        ) => settings
            .net
            .with_context(|| format!("zone '{identity}' has no copper net evidence"))?,
        _ => bail!("zone '{identity}' has unknown or inconsistent type/settings"),
    };
    ensure!(
        net.code.as_ref().is_none_or(|code| code.value >= 0),
        "zone '{identity}' has an invalid copper net code"
    );
    if !net.name.is_empty() {
        return Ok(Some(net));
    }
    ensure!(
        net.code.as_ref().is_some_and(|code| code.value == 0),
        "zone '{identity}' has no copper net name or explicit unconnected net"
    );
    Ok(None)
}

#[cfg(test)]
pub(super) mod test_support {
    use crate::test_support::MockIpcServer;
    use konnect_ipc::{builders::pack_any, gen::kiapi};
    use prost::Message;
    use std::sync::{Arc, Mutex};

    pub fn copper(net: &str, code: i32) -> prost_types::Any {
        pack_any(
            &kiapi::board::types::Zone {
                id: Some(kiapi::common::types::Kiid {
                    value: "zone-copper".to_string(),
                }),
                r#type: kiapi::board::types::ZoneType::ZtCopper as i32,
                settings: Some(kiapi::board::types::zone::Settings::CopperSettings(
                    kiapi::board::types::CopperZoneSettings {
                        net: Some(kiapi::board::types::Net {
                            name: net.to_string(),
                            code: Some(kiapi::board::types::NetCode { value: code }),
                        }),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            },
            "kiapi.board.types.Zone",
        )
    }

    pub fn rule_area() -> prost_types::Any {
        pack_any(
            &kiapi::board::types::Zone {
                id: Some(kiapi::common::types::Kiid {
                    value: "zone-keepout".to_string(),
                }),
                r#type: kiapi::board::types::ZoneType::ZtRuleArea as i32,
                settings: Some(kiapi::board::types::zone::Settings::RuleAreaSettings(
                    kiapi::board::types::RuleAreaSettings {
                        keepout_copper: true,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            },
            "kiapi.board.types.Zone",
        )
    }

    pub fn captured_zones() -> Vec<prost_types::Any> {
        [
            include_bytes!("../../tests/fixtures/issue_474_copper_zone_0.ipc.bin").as_slice(),
            include_bytes!("../../tests/fixtures/issue_474_zone_0.ipc.bin").as_slice(),
        ]
        .into_iter()
        .map(|value| prost_types::Any {
            type_url: "type.googleapis.com/kiapi.board.types.Zone".to_string(),
            value: value.to_vec(),
        })
        .collect()
    }

    pub fn serializer_shaped_graphical_zones() -> Vec<prost_types::Any> {
        let mut zone =
            kiapi::board::types::Zone::decode(captured_zones()[0].value.as_slice()).unwrap();
        zone.r#type = kiapi::board::types::ZoneType::ZtGraphical as i32;
        zone.layers = vec![kiapi::board::types::BoardLayer::BlFSilkS as i32];
        [
            Some(kiapi::board::types::Net {
                name: "GND".to_string(),
                ..Default::default()
            }),
            None,
            Some(kiapi::board::types::Net {
                name: "UNUSED".to_string(),
                code: Some(kiapi::board::types::NetCode { value: -1 }),
            }),
        ]
        .into_iter()
        .map(|net| {
            let Some(kiapi::board::types::zone::Settings::CopperSettings(settings)) =
                &mut zone.settings
            else {
                panic!("captured copper settings")
            };
            settings.net = net;
            pack_any(&zone, "kiapi.board.types.Zone")
        })
        .collect()
    }

    pub fn missing_net() -> prost_types::Any {
        let mut item =
            kiapi::board::types::Zone::decode(copper("GND", 1).value.as_slice()).unwrap();
        if let Some(kiapi::board::types::zone::Settings::CopperSettings(settings)) =
            &mut item.settings
        {
            settings.net = None;
        }
        pack_any(&item, "kiapi.board.types.Zone")
    }

    pub fn invalid_zones() -> Vec<prost_types::Any> {
        let source = copper("GND", 1);
        let zone = kiapi::board::types::Zone::decode(source.value.as_slice()).unwrap();
        let mut missing_settings = zone.clone();
        missing_settings.settings = None;
        let mut mismatched = zone.clone();
        mismatched.r#type = kiapi::board::types::ZoneType::ZtRuleArea as i32;
        let mut unknown = zone.clone();
        unknown.r#type = 99;
        let mut unknown_net = zone.clone();
        if let Some(kiapi::board::types::zone::Settings::CopperSettings(settings)) =
            &mut unknown_net.settings
        {
            settings.net = Some(kiapi::board::types::Net::default());
        }
        let mut wrong_item = source;
        wrong_item.type_url = "type.googleapis.com/kiapi.board.types.Track".to_string();
        vec![
            missing_net(),
            pack_any(&missing_settings, "kiapi.board.types.Zone"),
            pack_any(&mismatched, "kiapi.board.types.Zone"),
            pack_any(&unknown, "kiapi.board.types.Zone"),
            pack_any(&unknown_net, "kiapi.board.types.Zone"),
            copper("", 1),
            copper("GND", -1),
            wrong_item,
            prost_types::Any {
                type_url: "type.googleapis.com/kiapi.board.types.Zone".to_string(),
                value: vec![0xff],
            },
        ]
    }

    pub fn mock_zones(
        board: &std::path::Path,
        zones: Vec<prost_types::Any>,
    ) -> (MockIpcServer, Arc<Mutex<Vec<String>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let server =
            super::super::pcb_board::board_mock::spawn_kicad_holding_board(board, move |command| {
                observed.lock().unwrap().push(command.type_url.clone());
                if command.type_url.ends_with("GetItems") {
                    let request =
                        kiapi::common::commands::GetItems::decode(command.value.as_slice())
                            .unwrap();
                    let items = if request
                        .types
                        .contains(&(kiapi::common::types::KiCadObjectType::KotPcbZone as i32))
                    {
                        zones.clone()
                    } else {
                        Vec::new()
                    };
                    return Some(pack_any(
                        &kiapi::common::commands::GetItemsResponse {
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            items,
                            ..Default::default()
                        },
                        "kiapi.common.commands.GetItemsResponse",
                    ));
                }
                if command.type_url.ends_with("GetNets") {
                    return Some(pack_any(
                        &kiapi::board::commands::NetsResponse {
                            nets: [(1, "GND"), (2, "SIG_A"), (3, "SIG_B")]
                                .into_iter()
                                .map(|(code, name)| kiapi::board::types::Net {
                                    code: Some(kiapi::board::types::NetCode { value: code }),
                                    name: name.to_string(),
                                })
                                .collect(),
                        },
                        "kiapi.board.commands.NetsResponse",
                    ));
                }
                if command.type_url.ends_with("GetBoundingBox") {
                    return Some(super::super::pcb_board::board_mock::kicad_bounding_boxes(
                        command,
                        |_| (0.0, 0.0, 50.0, 40.0),
                    ));
                }
                None
            });
        (server, requests)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{
        captured_zones, copper, invalid_zones, rule_area, serializer_shaped_graphical_zones,
    };
    use super::*;

    #[test]
    fn real_kicad_zone_capture_supplies_a_name_without_a_net_code() {
        let zones = captured_zones();
        let net = routed_net(&zones[0]).unwrap().unwrap();
        assert_eq!(net.name, "GND");
        assert_eq!(net.code, None);
        assert_eq!(routed_net(&zones[1]).unwrap(), None);
    }

    #[test]
    fn classify_known_zone_types_and_unconnected_copper() {
        assert_eq!(routed_net(&rule_area()).unwrap(), None);
        assert_eq!(routed_net(&copper("", 0)).unwrap(), None);
        assert_eq!(routed_net(&copper("GND", 1)).unwrap().unwrap().name, "GND");
        let mut teardrop = Zone::decode(copper("SIG_A", 2).value.as_slice()).unwrap();
        teardrop.r#type = ZoneType::ZtTeardrop as i32;
        let item = konnect_ipc::builders::pack_any(&teardrop, "kiapi.board.types.Zone");
        assert_eq!(routed_net(&item).unwrap().unwrap().name, "SIG_A");
        let graphical = Zone {
            r#type: ZoneType::ZtGraphical as i32,
            ..Default::default()
        };
        let item = konnect_ipc::builders::pack_any(&graphical, "kiapi.board.types.Zone");
        assert_eq!(routed_net(&item).unwrap(), None);
    }

    #[test]
    fn graphical_zone_ignores_serializer_copper_settings_and_stale_nets() {
        for zone in serializer_shaped_graphical_zones() {
            assert_eq!(routed_net(&zone).unwrap(), None);
        }
    }

    #[test]
    fn unavailable_zone_evidence_is_not_an_empty_routed_net() {
        for item in invalid_zones() {
            let error = routed_net(&item).unwrap_err();
            assert!(error.to_string().contains("zone"), "{error:#}");
            if let Ok(zone) = Zone::decode(item.value.as_slice()) {
                if konnect_ipc::builders::any_is(&item, "kiapi.board.types.Zone") {
                    assert!(
                        error.to_string().contains(&zone.id.unwrap().value),
                        "{error:#}"
                    );
                }
            }
        }
    }
}
