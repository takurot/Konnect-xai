use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use prost::Message;

use crate::gen::kiapi;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PadChamfer {
    number: String,
    shape: i32,
    ratio_bits: u64,
    corner_mask: u8,
}

#[derive(Debug, Clone, Default)]
pub struct FootprintChamfers(BTreeMap<String, Vec<PadChamfer>>);

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
                for layer in stack.copper_layers {
                    let corners = layer.chamfered_corners.as_ref().map_or(0, |corners| {
                        u8::from(corners.top_left)
                            | (u8::from(corners.top_right) << 1)
                            | (u8::from(corners.bottom_left) << 2)
                            | (u8::from(corners.bottom_right) << 3)
                    });
                    if layer.shape == kiapi::board::types::PadStackShape::PssChamferedrect as i32
                        || layer.chamfer_ratio != 0.0
                        || corners != 0
                    {
                        if !layer.chamfer_ratio.is_finite() {
                            bail!(
                                "footprint '{reference}' pad '{}' has an invalid chamfer ratio",
                                pad.number
                            );
                        }
                        let ratio = if layer.chamfer_ratio == 0.0 {
                            0
                        } else {
                            layer.chamfer_ratio.to_bits()
                        };
                        chamfers.push(PadChamfer {
                            number: pad.number.clone(),
                            shape: layer.shape,
                            ratio_bits: ratio,
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
                if footprints.insert(reference.clone(), chamfers).is_some() {
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
        let observed = Self::from_items(items)?;
        for (reference, expected) in &self.0 {
            if observed.0.get(reference) != Some(expected) {
                bail!("footprint '{reference}' chamfer ratio, corners or pad multiplicity differ from what was sent");
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
                        pad_stack: Some(kiapi::board::types::PadStack {
                            copper_layers: vec![kiapi::board::types::PadStackLayer {
                                shape: kiapi::board::types::PadStackShape::PssChamferedrect as i32,
                                chamfer_ratio: 0.2,
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
    fn chamfer_verification_refuses_unreadable_or_ambiguous_evidence() {
        let item = footprint(&[1]);
        assert!(FootprintChamfers::from_items([&item, &item]).is_err());
        let mut unreadable = item.clone();
        unreadable.value = vec![0xff];
        assert!(FootprintChamfers::from_items([&unreadable]).is_err());
        let mut missing_reference =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        missing_reference.reference_field = None;
        let missing_reference =
            crate::builders::pack_any(&missing_reference, "kiapi.board.types.FootprintInstance");
        assert!(FootprintChamfers::from_items([&missing_reference]).is_err());
    }
}
