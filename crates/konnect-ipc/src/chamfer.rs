use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use prost::Message;

use crate::gen::kiapi;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PadChamfer {
    number: String,
    position: (i64, i64),
    angle_bits: u64,
    layers: Vec<i32>,
    layer: i32,
    size: (i64, i64),
    offset: (i64, i64),
    shape: i32,
    ratio_bits: u64,
    rounding_ratio_bits: u64,
    corner_mask: u8,
}

#[derive(Debug, Clone, Default)]
pub struct FootprintChamfers(BTreeMap<String, ChamferFootprint>);

#[derive(Debug, Clone)]
struct ChamferFootprint {
    id: Option<String>,
    pads: Vec<PadChamfer>,
}

impl FootprintChamfers {
    pub fn from_items<'a>(items: impl IntoIterator<Item = &'a prost_types::Any>) -> Result<Self> {
        let mut footprints = BTreeMap::new();
        for item in items {
            if !crate::builders::any_is(item, "kiapi.board.types.FootprintInstance") {
                continue;
            }
            let footprint = kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                .context("unreadable footprint during chamfer verification")?;
            let reference = crate::client::footprint_reference(&footprint).to_string();
            let Some(definition) = footprint.definition.as_ref() else {
                continue;
            };
            let mut chamfers = Vec::new();
            for item in &definition.items {
                if !crate::builders::any_is(item, "kiapi.board.types.Pad") {
                    continue;
                }
                let pad = kiapi::board::types::Pad::decode(item.value.as_slice())
                    .with_context(|| format!("footprint '{reference}' has an unreadable pad"))?;
                let Some(stack) = pad.pad_stack else {
                    continue;
                };
                for layer in &stack.copper_layers {
                    let corners = layer.chamfered_corners.as_ref().map_or(0, |corners| {
                        u8::from(corners.top_left)
                            | (u8::from(corners.top_right) << 1)
                            | (u8::from(corners.bottom_left) << 2)
                            | (u8::from(corners.bottom_right) << 3)
                    });
                    if layer.shape == kiapi::board::types::PadStackShape::PssChamferedrect as i32 {
                        if !layer.chamfer_ratio.is_finite()
                            || !layer.corner_rounding_ratio.is_finite()
                        {
                            bail!(
                                "footprint '{reference}' pad '{}' has an invalid chamfer or rounding ratio",
                                pad.number
                            );
                        }
                        let ratio = if layer.chamfer_ratio == 0.0 {
                            0
                        } else {
                            layer.chamfer_ratio.to_bits()
                        };
                        let position = pad.position.as_ref().with_context(|| {
                            format!(
                                "footprint '{reference}' pad '{}' has no position",
                                pad.number
                            )
                        })?;
                        let size = layer.size.as_ref().with_context(|| {
                            format!("footprint '{reference}' pad '{}' has no size", pad.number)
                        })?;
                        let angle = stack
                            .angle
                            .as_ref()
                            .map_or(0.0, |angle| angle.value_degrees);
                        if !angle.is_finite() {
                            bail!(
                                "footprint '{reference}' pad '{}' has an invalid angle",
                                pad.number
                            );
                        }
                        let angle = angle.rem_euclid(360.0);
                        let mut layers = stack.layers.clone();
                        layers.sort_unstable();
                        chamfers.push(PadChamfer {
                            number: pad.number.clone(),
                            position: (position.x_nm, position.y_nm),
                            angle_bits: if angle == 0.0 { 0 } else { angle.to_bits() },
                            layers,
                            layer: layer.layer,
                            size: (size.x_nm, size.y_nm),
                            offset: layer
                                .offset
                                .as_ref()
                                .map_or((0, 0), |offset| (offset.x_nm, offset.y_nm)),
                            shape: layer.shape,
                            ratio_bits: ratio,
                            rounding_ratio_bits: if layer.corner_rounding_ratio == 0.0 {
                                0
                            } else {
                                layer.corner_rounding_ratio.to_bits()
                            },
                            corner_mask: corners,
                        });
                    }
                }
            }
            if !chamfers.is_empty() {
                if reference.is_empty() {
                    bail!("a chamfered footprint has no reference");
                }
                chamfers.sort_unstable();
                let evidence = ChamferFootprint {
                    id: footprint
                        .id
                        .as_ref()
                        .filter(|id| !id.value.is_empty())
                        .map(|id| id.value.clone()),
                    pads: chamfers,
                };
                if footprints.insert(reference.clone(), evidence).is_some() {
                    bail!("footprint reference '{reference}' is duplicated during chamfer verification");
                }
            }
        }
        Ok(Self(footprints))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn verify(&self, items: &[prost_types::Any]) -> Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        let footprints = items
            .iter()
            .filter(|item| crate::builders::any_is(item, "kiapi.board.types.FootprintInstance"))
            .map(|item| {
                let footprint =
                    kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                        .context("unreadable footprint during chamfer verification")?;
                Ok((item, footprint))
            })
            .collect::<Result<Vec<_>>>()?;
        for (reference, expected) in &self.0 {
            let mut matching = footprints.iter().filter(|(_, footprint)| {
                if let Some(expected_id) = &expected.id {
                    footprint
                        .id
                        .as_ref()
                        .is_some_and(|id| id.value == *expected_id)
                } else {
                    crate::client::footprint_reference(footprint) == reference
                }
            });
            let Some((item, _)) = matching.next() else {
                bail!("footprint '{reference}' chamfer readback is missing");
            };
            if matching.next().is_some() {
                bail!("footprint '{reference}' is duplicated during chamfer verification");
            }
            let observed = Self::from_items([*item])?;
            let Some(observed) = observed.0.get(reference) else {
                bail!("footprint '{reference}' chamfer readback is missing");
            };
            if observed.pads != expected.pads
                || (expected.id.is_some() && observed.id != expected.id)
            {
                bail!("footprint '{reference}' chamfer geometry or identity differs from what was sent");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn footprint(corners: &[u8]) -> prost_types::Any {
        let items = corners
            .iter()
            .map(|mask| {
                crate::builders::pack_any(
                    &kiapi::board::types::Pad {
                        number: "1".to_string(),
                        position: Some(Default::default()),
                        pad_stack: Some(kiapi::board::types::PadStack {
                            layers: vec![kiapi::board::types::BoardLayer::BlFCu as i32],
                            copper_layers: vec![kiapi::board::types::PadStackLayer {
                                layer: kiapi::board::types::BoardLayer::BlFCu as i32,
                                shape: kiapi::board::types::PadStackShape::PssChamferedrect as i32,
                                chamfer_ratio: 0.2,
                                size: Some(kiapi::common::types::Vector2 {
                                    x_nm: 2_000_000,
                                    y_nm: 1_000_000,
                                }),
                                chamfered_corners: Some(
                                    kiapi::board::types::ChamferedRectCorners {
                                        top_left: mask & 1 != 0,
                                        top_right: mask & 2 != 0,
                                        bottom_left: mask & 4 != 0,
                                        bottom_right: mask & 8 != 0,
                                    },
                                ),
                                ..Default::default()
                            }],
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    "kiapi.board.types.Pad",
                )
            })
            .collect();
        crate::builders::pack_any(
            &kiapi::board::types::FootprintInstance {
                reference_field: Some(kiapi::board::types::Field {
                    text: Some(kiapi::board::types::BoardText {
                        text: Some(kiapi::common::types::Text {
                            text: "U1".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                definition: Some(kiapi::board::types::Footprint {
                    items,
                    ..Default::default()
                }),
                ..Default::default()
            },
            "kiapi.board.types.FootprintInstance",
        )
    }

    #[test]
    fn chamfer_verification_compares_duplicate_pad_multisets() {
        let expected = FootprintChamfers::from_items([&footprint(&[1, 8])]).unwrap();
        assert!(expected.verify(&[footprint(&[8, 1])]).is_ok());
        assert!(expected.verify(&[footprint(&[1])]).is_err());
        assert!(expected.verify(&[footprint(&[1, 1])]).is_err());
        assert!(expected.verify(&[footprint(&[1, 0])]).is_err());
        assert!(expected.verify(&[]).is_err());
    }
    #[test]
    fn chamfer_verification_rejects_a_changed_ratio() {
        let item = footprint(&[1]);
        let expected = FootprintChamfers::from_items([&item]).unwrap();
        let mut changed =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        let child = &mut changed.definition.as_mut().unwrap().items[0];
        let mut pad = kiapi::board::types::Pad::decode(child.value.as_slice()).unwrap();
        pad.pad_stack.as_mut().unwrap().copper_layers[0].chamfer_ratio = 0.3;
        *child = crate::builders::pack_any(&pad, "kiapi.board.types.Pad");
        assert!(expected
            .verify(&[crate::builders::pack_any(
                &changed,
                "kiapi.board.types.FootprintInstance"
            )])
            .is_err());
    }

    #[test]
    fn native_inactive_chamfer_defaults_are_not_chamfer_geometry() {
        let item = prost_types::Any {
            type_url: "type.googleapis.com/kiapi.board.types.FootprintInstance".to_string(),
            value: include_bytes!("../../konnect-core/tests/fixtures/issue_474_r1.ipc.bin")
                .to_vec(),
        };
        let captured =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        let layers = captured
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|item| crate::builders::any_is(item, "kiapi.board.types.Pad"))
            .flat_map(|item| {
                kiapi::board::types::Pad::decode(item.value.as_slice())
                    .unwrap()
                    .pad_stack
                    .unwrap()
                    .copper_layers
            })
            .collect::<Vec<_>>();
        assert!(!layers.is_empty());
        assert!(layers.iter().all(|layer| {
            layer.shape != kiapi::board::types::PadStackShape::PssChamferedrect as i32
                && layer.chamfer_ratio == 0.2
        }));
        let expected = FootprintChamfers::from_items([&item]).unwrap();
        assert!(expected.is_empty());
        assert!(expected.verify(&[]).is_ok());
    }

    #[test]
    fn chamfer_verification_ignores_unrelated_footprints() {
        let selected = footprint(&[1]);
        let expected = FootprintChamfers::from_items([&selected]).unwrap();
        let mut other =
            kiapi::board::types::FootprintInstance::decode(selected.value.as_slice()).unwrap();
        other
            .reference_field
            .as_mut()
            .unwrap()
            .text
            .as_mut()
            .unwrap()
            .text
            .as_mut()
            .unwrap()
            .text = "OTHER".to_string();
        let unrelated = crate::builders::pack_any(&other, "kiapi.board.types.FootprintInstance");
        other.definition.as_mut().unwrap().items[0].value = vec![0xff];
        let malformed = crate::builders::pack_any(&other, "kiapi.board.types.FootprintInstance");
        assert!(expected
            .verify(&[selected, unrelated.clone(), unrelated, malformed])
            .is_ok());
    }

    #[test]
    fn chamfer_verification_selects_existing_kiid_before_reference() {
        let mut selected =
            kiapi::board::types::FootprintInstance::decode(footprint(&[1]).value.as_slice())
                .unwrap();
        selected.id = Some(kiapi::common::types::Kiid {
            value: "selected".to_string(),
        });
        let item = crate::builders::pack_any(&selected, "kiapi.board.types.FootprintInstance");
        let expected = FootprintChamfers::from_items([&item]).unwrap();
        let mut unrelated = selected.clone();
        unrelated.id.as_mut().unwrap().value = "unrelated".to_string();
        unrelated.definition.as_mut().unwrap().items[0].value = vec![0xff];
        let unrelated =
            crate::builders::pack_any(&unrelated, "kiapi.board.types.FootprintInstance");
        assert!(expected.verify(&[unrelated, item.clone()]).is_ok());
        assert!(expected.verify(&[item.clone(), item.clone()]).is_err());
        selected.definition.as_mut().unwrap().items[0].value = vec![0xff];
        assert!(expected
            .verify(&[crate::builders::pack_any(
                &selected,
                "kiapi.board.types.FootprintInstance"
            )])
            .is_err());
    }

    #[test]
    fn chamfer_verification_rejects_lost_shape_ratio_or_corners() {
        let item = footprint(&[1]);
        let expected = FootprintChamfers::from_items([&item]).unwrap();
        for change in 0..3 {
            let mut value =
                kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
            let child = &mut value.definition.as_mut().unwrap().items[0];
            let mut pad = kiapi::board::types::Pad::decode(child.value.as_slice()).unwrap();
            let layer = &mut pad.pad_stack.as_mut().unwrap().copper_layers[0];
            match change {
                0 => layer.shape = kiapi::board::types::PadStackShape::PssRoundrect as i32,
                1 => layer.chamfer_ratio = 0.0,
                _ => layer.chamfered_corners = None,
            }
            *child = crate::builders::pack_any(&pad, "kiapi.board.types.Pad");
            assert!(
                expected
                    .verify(&[crate::builders::pack_any(
                        &value,
                        "kiapi.board.types.FootprintInstance",
                    )])
                    .is_err(),
                "change {change}"
            );
        }
        let unreadable = prost_types::Any {
            type_url: "type.googleapis.com/kiapi.board.types.FootprintInstance".to_string(),
            value: vec![0xff],
        };
        assert!(expected.verify(&[item.clone(), item]).is_err());
        assert!(expected.verify(&[unreadable]).is_err());
    }

    #[test]
    fn chamfer_verification_binds_corners_to_physical_pads() {
        let place = |item: prost_types::Any| {
            let mut footprint =
                kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
            for (index, child) in footprint
                .definition
                .as_mut()
                .unwrap()
                .items
                .iter_mut()
                .enumerate()
            {
                let mut pad = kiapi::board::types::Pad::decode(child.value.as_slice()).unwrap();
                pad.position = Some(kiapi::common::types::Vector2 {
                    x_nm: index as i64 * 1_000_000,
                    y_nm: 0,
                });
                *child = crate::builders::pack_any(&pad, "kiapi.board.types.Pad");
            }
            footprint
        };
        let expected = place(footprint(&[1, 8]));
        let mut reordered = expected.clone();
        reordered.definition.as_mut().unwrap().items.reverse();
        let swapped = place(footprint(&[8, 1]));
        let pack = |value| crate::builders::pack_any(&value, "kiapi.board.types.FootprintInstance");
        let expected = FootprintChamfers::from_items([&pack(expected)]).unwrap();
        assert!(expected.verify(&[pack(reordered)]).is_ok());
        assert!(expected.verify(&[pack(swapped)]).is_err());
    }

    #[test]
    fn chamfer_verification_binds_corners_to_coincident_pad_rounding() {
        let with_rounding = |item: prost_types::Any| {
            let mut footprint =
                kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
            for (index, child) in footprint
                .definition
                .as_mut()
                .unwrap()
                .items
                .iter_mut()
                .enumerate()
            {
                let mut pad = kiapi::board::types::Pad::decode(child.value.as_slice()).unwrap();
                pad.pad_stack.as_mut().unwrap().copper_layers[0].corner_rounding_ratio =
                    [0.1, 0.25][index];
                *child = crate::builders::pack_any(&pad, "kiapi.board.types.Pad");
            }
            crate::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
        };
        let item = with_rounding(footprint(&[1, 8]));
        let expected = FootprintChamfers::from_items([&item]).unwrap();
        let mut reordered =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        reordered.definition.as_mut().unwrap().items.reverse();
        assert!(expected
            .verify(&[crate::builders::pack_any(
                &reordered,
                "kiapi.board.types.FootprintInstance",
            )])
            .is_ok());
        assert!(expected
            .verify(&[with_rounding(footprint(&[8, 1]))])
            .is_err());
    }

    #[test]
    fn chamfer_verification_preserves_existing_footprint_identity() {
        let assign_id = |id: &str| {
            let item = footprint(&[1]);
            let mut value =
                kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
            value.id = Some(kiapi::common::types::Kiid {
                value: id.to_string(),
            });
            crate::builders::pack_any(&value, "kiapi.board.types.FootprintInstance")
        };
        let existing = assign_id("existing");
        let expected = FootprintChamfers::from_items([&existing]).unwrap();
        assert!(expected.verify(&[assign_id("existing")]).is_ok());
        assert!(expected.verify(&[assign_id("different")]).is_err());
        let creation = footprint(&[1]);
        let expected = FootprintChamfers::from_items([&creation]).unwrap();
        assert!(expected.verify(&[assign_id("assigned-by-kicad")]).is_ok());
    }

    #[test]
    fn chamfer_verification_binds_layer_and_pad_geometry() {
        let item = footprint(&[1]);
        let expected = FootprintChamfers::from_items([&item]).unwrap();
        for change in 0..7 {
            let mut value =
                kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
            let child = &mut value.definition.as_mut().unwrap().items[0];
            let mut pad = kiapi::board::types::Pad::decode(child.value.as_slice()).unwrap();
            match change {
                0 => pad.position = None,
                1 => {
                    pad.pad_stack.as_mut().unwrap().angle = Some(kiapi::common::types::Angle {
                        value_degrees: 90.0,
                    })
                }
                2 => pad
                    .pad_stack
                    .as_mut()
                    .unwrap()
                    .layers
                    .push(kiapi::board::types::BoardLayer::BlBCu as i32),
                3 => {
                    pad.pad_stack.as_mut().unwrap().copper_layers[0].layer =
                        kiapi::board::types::BoardLayer::BlBCu as i32
                }
                4 => pad.pad_stack.as_mut().unwrap().copper_layers[0].size = None,
                5 => {
                    pad.pad_stack.as_mut().unwrap().copper_layers[0]
                        .size
                        .as_mut()
                        .unwrap()
                        .x_nm += 1
                }
                _ => {
                    pad.pad_stack.as_mut().unwrap().copper_layers[0].offset =
                        Some(kiapi::common::types::Vector2 { x_nm: 1, y_nm: 0 })
                }
            }
            *child = crate::builders::pack_any(&pad, "kiapi.board.types.Pad");
            assert!(
                expected
                    .verify(&[crate::builders::pack_any(
                        &value,
                        "kiapi.board.types.FootprintInstance"
                    )])
                    .is_err(),
                "change {change}"
            );
        }
    }
    #[test]
    fn chamfer_verification_refuses_unreadable_or_ambiguous_evidence() {
        let item = footprint(&[1]);
        assert!(FootprintChamfers::from_items([&item, &item]).is_err());
        let mut unreadable = item.clone();
        unreadable.value = vec![0xff];
        assert!(FootprintChamfers::from_items([&unreadable]).is_err());
        let mut invalid_rounding =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        let child = &mut invalid_rounding.definition.as_mut().unwrap().items[0];
        let mut pad = kiapi::board::types::Pad::decode(child.value.as_slice()).unwrap();
        pad.pad_stack.as_mut().unwrap().copper_layers[0].corner_rounding_ratio = f64::NAN;
        *child = crate::builders::pack_any(&pad, "kiapi.board.types.Pad");
        let invalid_rounding =
            crate::builders::pack_any(&invalid_rounding, "kiapi.board.types.FootprintInstance");
        assert!(FootprintChamfers::from_items([&invalid_rounding]).is_err());
        let mut missing_reference =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        missing_reference.reference_field = None;
        let missing_reference =
            crate::builders::pack_any(&missing_reference, "kiapi.board.types.FootprintInstance");
        assert!(FootprintChamfers::from_items([&missing_reference]).is_err());
    }
}
