use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use prost::Message;

use crate::gen::kiapi::{board::types as board, common::types as common};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Presentation {
    kiid: Option<String>,
    library: Vec<u8>,
    position: Option<(i64, i64)>,
    rotation: u64,
    layer: i32,
    fields: BTreeMap<String, Vec<u8>>,
    strokes: Vec<(i32, Vec<u8>, i32, i64)>,
}

#[derive(Debug, Clone, Default)]
pub struct FootprintPresentation(BTreeMap<String, Presentation>);

pub fn normalize_footprint_field(field: &board::Field) -> Result<board::Field> {
    let mut field = field.clone();
    field.id = None;
    if let Some(board_text) = field.text.as_mut() {
        board_text.id = None;
        board_text.parent = None;
        if let Some(attributes) = board_text
            .text
            .as_mut()
            .and_then(|text| text.attributes.as_mut())
        {
            if !attributes.line_spacing.is_finite() {
                bail!("field '{}' has nonfinite line spacing", field.name);
            }
            if let Some(angle) = attributes.angle.as_mut() {
                if !angle.value_degrees.is_finite() {
                    bail!("field '{}' has a nonfinite angle", field.name);
                }
                angle.value_degrees = angle.value_degrees.rem_euclid(360.0);
                if angle.value_degrees == 0.0 {
                    attributes.angle = None;
                }
            }
        }
    }
    Ok(field)
}

pub fn normalize_footprint_graphic_geometry(shape: &mut common::GraphicShape) {
    match shape.geometry.as_mut() {
        Some(common::graphic_shape::Geometry::Rectangle(rectangle)) => {
            if rectangle
                .corner_radius
                .as_ref()
                .is_some_and(|radius| radius.value_nm == 0)
            {
                rectangle.corner_radius = None;
            }
        }
        Some(common::graphic_shape::Geometry::Polygon(polyset)) => normalize_polyset(polyset),
        Some(common::graphic_shape::Geometry::Arc(arc)) => {
            if let (Some(start), Some(end)) = (arc.start.as_ref(), arc.end.as_ref()) {
                if (start.x_nm, start.y_nm) > (end.x_nm, end.y_nm) {
                    std::mem::swap(&mut arc.start, &mut arc.end);
                }
            }
        }
        _ => {}
    }
}

fn normalize_polyset(polyset: &mut common::PolySet) {
    for polygon in &mut polyset.polygons {
        if let Some(outline) = polygon.outline.as_mut() {
            normalize_polyline(outline);
        }
        for hole in &mut polygon.holes {
            normalize_polyline(hole);
        }
        polygon.holes.sort_by_key(prost::Message::encode_to_vec);
    }
    polyset.polygons.sort_by_key(prost::Message::encode_to_vec);
}

fn normalize_polyline(polyline: &mut common::PolyLine) {
    if !polyline.closed || polyline.nodes.len() < 2 {
        return;
    }
    polyline.nodes.dedup_by(|a, b| {
        matches!(
            (a.geometry.as_ref(), b.geometry.as_ref()),
            (
                Some(common::poly_line_node::Geometry::Point(a)),
                Some(common::poly_line_node::Geometry::Point(b))
            ) if a == b
        )
    });
    if polyline.nodes.len() > 1
        && polyline.nodes.first() == polyline.nodes.last()
        && matches!(
            polyline
                .nodes
                .first()
                .and_then(|node| node.geometry.as_ref()),
            Some(common::poly_line_node::Geometry::Point(_))
        )
    {
        polyline.nodes.pop();
    }
    let candidates = [
        polyline.nodes.clone(),
        polyline.nodes.iter().cloned().rev().collect(),
    ];
    let mut best: Option<(Vec<u8>, Vec<common::PolyLineNode>)> = None;
    for nodes in candidates {
        for offset in 0..nodes.len() {
            let rotated = nodes[offset..]
                .iter()
                .chain(nodes[..offset].iter())
                .cloned()
                .collect::<Vec<_>>();
            let encoded = rotated
                .iter()
                .flat_map(prost::Message::encode_to_vec)
                .collect::<Vec<_>>();
            if best
                .as_ref()
                .is_none_or(|(best_encoded, _)| encoded < *best_encoded)
            {
                best = Some((encoded, rotated));
            }
        }
    }
    if let Some((_, nodes)) = best {
        polyline.nodes = nodes;
    }
}

impl Presentation {
    fn of(footprint: &board::FootprintInstance) -> Result<Self> {
        let definition = footprint
            .definition
            .as_ref()
            .context("footprint presentation has no definition")?;
        let check_parent = |parent: Option<&common::Kiid>| -> Result<()> {
            if let (Some(parent), Some(id)) = (parent, footprint.id.as_ref()) {
                if parent.value != id.value {
                    bail!("footprint child refers to a different parent UUID");
                }
            }
            Ok(())
        };
        let mut fields = BTreeMap::new();
        for (name, primary, fallback) in [
            (
                "Reference",
                &footprint.reference_field,
                &definition.reference_field,
            ),
            ("Value", &footprint.value_field, &definition.value_field),
            (
                "Datasheet",
                &footprint.datasheet_field,
                &definition.datasheet_field,
            ),
            (
                "Description",
                &footprint.description_field,
                &definition.description_field,
            ),
        ] {
            if let Some(field) = primary.as_ref().or(fallback.as_ref()) {
                check_parent(field.text.as_ref().and_then(|text| text.parent.as_ref()))?;
                fields.insert(
                    name.into(),
                    normalize_footprint_field(field)?.encode_to_vec(),
                );
            }
        }
        let mut strokes = Vec::new();
        for item in &definition.items {
            if crate::builders::any_is(item, "kiapi.board.types.Field") {
                let field = board::Field::decode(item.value.as_slice())
                    .context("unreadable custom field during footprint verification")?;
                check_parent(field.text.as_ref().and_then(|text| text.parent.as_ref()))?;
                if field.name.is_empty()
                    || fields
                        .insert(
                            field.name.clone(),
                            normalize_footprint_field(&field)?.encode_to_vec(),
                        )
                        .is_some()
                {
                    bail!(
                        "duplicate or unnamed field '{}' during footprint verification",
                        field.name
                    );
                }
            } else if crate::builders::any_is(item, "kiapi.board.types.BoardGraphicShape") {
                let item = board::BoardGraphicShape::decode(item.value.as_slice())
                    .context("unreadable footprint graphic during verification")?;
                check_parent(item.parent.as_ref())?;
                let mut shape = item.shape.context("footprint graphic has no geometry")?;
                let attributes = shape
                    .attributes
                    .take()
                    .context("footprint graphic has no attributes")?;
                let stroke = attributes
                    .stroke
                    .context("footprint graphic has no stroke")?;
                let style = match common::StrokeLineStyle::try_from(stroke.style)
                    .context("unknown footprint stroke enum")?
                {
                    common::StrokeLineStyle::SlsUnknown
                    | common::StrokeLineStyle::SlsDefault
                    | common::StrokeLineStyle::SlsSolid => common::StrokeLineStyle::SlsSolid as i32,
                    other => other as i32,
                };
                normalize_footprint_graphic_geometry(&mut shape);
                strokes.push((
                    item.layer,
                    shape.encode_to_vec(),
                    style,
                    stroke.width.map_or(0, |width| width.value_nm),
                ));
            }
        }
        strokes.sort();
        let rotation = footprint
            .orientation
            .as_ref()
            .map_or(0.0, |angle| angle.value_degrees);
        if !rotation.is_finite() {
            bail!("footprint has a nonfinite orientation");
        }
        Ok(Self {
            kiid: footprint.id.as_ref().map(|id| id.value.clone()),
            library: definition
                .id
                .as_ref()
                .map_or_else(Vec::new, Message::encode_to_vec),
            position: footprint
                .position
                .as_ref()
                .map(|point| (point.x_nm, point.y_nm)),
            rotation: {
                let value = rotation.rem_euclid(360.0);
                if value == 0.0 {
                    0
                } else {
                    value.to_bits()
                }
            },
            layer: footprint.layer,
            fields,
            strokes,
        })
    }
}

impl FootprintPresentation {
    pub fn from_items<'a>(items: impl IntoIterator<Item = &'a prost_types::Any>) -> Result<Self> {
        let mut expected = BTreeMap::new();
        for item in items {
            if !crate::builders::any_is(item, "kiapi.board.types.FootprintInstance") {
                continue;
            }
            let footprint = board::FootprintInstance::decode(item.value.as_slice())
                .context("unreadable footprint during presentation verification")?;
            let reference = crate::client::footprint_reference(&footprint).to_string();
            if reference.is_empty()
                || expected
                    .insert(reference.clone(), Presentation::of(&footprint)?)
                    .is_some()
            {
                bail!("empty or duplicate footprint reference '{reference}' during presentation verification");
            }
        }
        Ok(Self(expected))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn verify(&self, items: &[prost_types::Any]) -> Result<()> {
        let mut observed = BTreeMap::new();
        for item in items {
            if !crate::builders::any_is(item, "kiapi.board.types.FootprintInstance") {
                continue;
            }
            let footprint = board::FootprintInstance::decode(item.value.as_slice())
                .context("unreadable footprint during presentation readback")?;
            let reference = crate::client::footprint_reference(&footprint).to_string();
            if self.0.contains_key(&reference)
                && observed.insert(reference.clone(), footprint).is_some()
            {
                bail!("footprint '{reference}' is ambiguous during presentation readback");
            }
        }
        for (reference, expected) in &self.0 {
            let footprint = observed.get(reference).with_context(|| {
                format!("footprint '{reference}' is missing from presentation readback")
            })?;
            let mut actual = Presentation::of(footprint)?;
            if expected.kiid.is_none() {
                actual.kiid = None;
            }
            if &actual != expected {
                bail!("footprint '{reference}' identity, field presentation or graphic stroke differs from what was sent");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> prost_types::Any {
        let custom = board::Field {
            name: "AssemblyVendor".into(),
            visible: false,
            text: Some(board::BoardText {
                layer: board::BoardLayer::BlFFab as i32,
                text: Some(common::Text {
                    text: "Example".into(),
                    position: Some(crate::builders::vec2(1.0, 2.0)),
                    attributes: Some(common::TextAttributes {
                        size: Some(crate::builders::vec2(0.8, 0.9)),
                        stroke_width: Some(crate::builders::distance(0.12)),
                        line_spacing: 1.0,
                        keep_upright: true,
                        multiline: true,
                        visible: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let graphic = crate::IpcGraphicDefinition::Line {
            start: (0.0, 0.0),
            end: (1.0, 1.0),
            layer: "F.CrtYd".into(),
            width: 0.1,
            style: common::StrokeLineStyle::SlsDash,
        };
        let item = crate::KiCadIpcClient::build_footprint_item(
            "Test:Socket",
            "U1",
            "10k",
            &[],
            &[graphic],
            &crate::IpcFieldPlacement {
                fields: vec![custom],
                ..Default::default()
            },
            10.0,
            20.0,
            0.0,
            "F.Cu",
        )
        .unwrap();
        let mut footprint = board::FootprintInstance::decode(item.value.as_slice()).unwrap();
        footprint.id = Some(common::Kiid {
            value: "target-footprint".into(),
        });
        crate::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
    }

    #[test]
    fn presentation_checks_custom_effects_visibility_and_layer() {
        let wanted = item();
        let expected = FootprintPresentation::from_items([&wanted]).unwrap();
        for change in 0..8 {
            let mut footprint = board::FootprintInstance::decode(wanted.value.as_slice()).unwrap();
            let child = footprint
                .definition
                .as_mut()
                .unwrap()
                .items
                .iter_mut()
                .find(|item| crate::builders::any_is(item, "kiapi.board.types.Field"))
                .unwrap();
            let mut field = board::Field::decode(child.value.as_slice()).unwrap();
            match change {
                0 => field.visible = !field.visible,
                1 => field.text.as_mut().unwrap().layer = board::BoardLayer::BlFSilkS as i32,
                2 => {
                    field
                        .text
                        .as_mut()
                        .unwrap()
                        .text
                        .as_mut()
                        .unwrap()
                        .attributes
                        .as_mut()
                        .unwrap()
                        .size
                        .as_mut()
                        .unwrap()
                        .x_nm += 1
                }
                3 => {
                    field
                        .text
                        .as_mut()
                        .unwrap()
                        .text
                        .as_mut()
                        .unwrap()
                        .attributes
                        .as_mut()
                        .unwrap()
                        .stroke_width
                        .as_mut()
                        .unwrap()
                        .value_nm += 1
                }
                4 => {
                    field
                        .text
                        .as_mut()
                        .unwrap()
                        .text
                        .as_mut()
                        .unwrap()
                        .attributes
                        .as_mut()
                        .unwrap()
                        .keep_upright = false
                }
                5 => {
                    field
                        .text
                        .as_mut()
                        .unwrap()
                        .text
                        .as_mut()
                        .unwrap()
                        .attributes
                        .as_mut()
                        .unwrap()
                        .line_spacing = 1.2
                }
                6 => {
                    field
                        .text
                        .as_mut()
                        .unwrap()
                        .text
                        .as_mut()
                        .unwrap()
                        .position
                        .as_mut()
                        .unwrap()
                        .x_nm += 1
                }
                _ => field.text.as_mut().unwrap().text.as_mut().unwrap().text = "changed".into(),
            }
            *child = crate::builders::pack_any(&field, "kiapi.board.types.Field");
            let actual =
                crate::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance");
            assert!(
                expected.verify(&[actual]).is_err(),
                "missed presentation change {change}"
            );
        }
    }

    #[test]
    fn presentation_binds_styles_to_their_geometry_and_layer() {
        let wanted = item();
        let expected = FootprintPresentation::from_items([&wanted]).unwrap();
        for change in 0..3 {
            let mut footprint = board::FootprintInstance::decode(wanted.value.as_slice()).unwrap();
            let child = footprint
                .definition
                .as_mut()
                .unwrap()
                .items
                .iter_mut()
                .find(|item| crate::builders::any_is(item, "kiapi.board.types.BoardGraphicShape"))
                .unwrap();
            let mut graphic = board::BoardGraphicShape::decode(child.value.as_slice()).unwrap();
            match change {
                0 => {
                    graphic
                        .shape
                        .as_mut()
                        .unwrap()
                        .attributes
                        .as_mut()
                        .unwrap()
                        .stroke
                        .as_mut()
                        .unwrap()
                        .style = common::StrokeLineStyle::SlsSolid as i32
                }
                1 => graphic.layer = board::BoardLayer::BlFFab as i32,
                _ => {
                    let Some(common::graphic_shape::Geometry::Segment(segment)) =
                        graphic.shape.as_mut().unwrap().geometry.as_mut()
                    else {
                        panic!("expected line geometry");
                    };
                    segment.start.as_mut().unwrap().x_nm += 1;
                }
            }
            *child = crate::builders::pack_any(&graphic, "kiapi.board.types.BoardGraphicShape");
            let actual =
                crate::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance");
            assert!(
                expected.verify(&[actual]).is_err(),
                "missed stroke change {change}"
            );
        }
    }

    #[test]
    fn presentation_accepts_native_zero_rectangle_radius_but_checks_nonzero_radius() {
        let mut footprint = board::FootprintInstance::decode(item().value.as_slice()).unwrap();
        let rectangle = crate::builders::board_rectangle("F.CrtYd", 0.1, 0.0, 0.0, 1.0, 2.0, false);
        footprint.definition.as_mut().unwrap().items = vec![crate::builders::pack_any(
            &rectangle,
            "kiapi.board.types.BoardGraphicShape",
        )];
        let wanted = crate::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance");
        let expected = FootprintPresentation::from_items([&wanted]).unwrap();
        for radius in [0, 1] {
            let mut graphic = rectangle.clone();
            let Some(common::graphic_shape::Geometry::Rectangle(rectangle)) =
                graphic.shape.as_mut().unwrap().geometry.as_mut()
            else {
                panic!("expected rectangle geometry");
            };
            rectangle.corner_radius = Some(common::Distance { value_nm: radius });
            footprint.definition.as_mut().unwrap().items[0] =
                crate::builders::pack_any(&graphic, "kiapi.board.types.BoardGraphicShape");
            let actual =
                crate::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance");
            assert_eq!(expected.verify(&[actual]).is_ok(), radius == 0);
        }
    }

    #[test]
    fn presentation_accepts_native_polygon_point_dedup_but_checks_vertices() {
        let points = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)];
        let polygon_item = |points: &[(f64, f64)]| {
            let mut footprint = board::FootprintInstance::decode(item().value.as_slice()).unwrap();
            let graphic = crate::builders::board_polygon("F.CrtYd", 0.1, false, &[points.to_vec()]);
            footprint.definition.as_mut().unwrap().items = vec![crate::builders::pack_any(
                &graphic,
                "kiapi.board.types.BoardGraphicShape",
            )];
            crate::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
        };
        for repeated in [
            vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)],
            vec![(0.0, 0.0), (1.0, 0.0), (1.0, 0.0), (1.0, 1.0)],
        ] {
            let wanted = polygon_item(&repeated);
            let expected = FootprintPresentation::from_items([&wanted]).unwrap();
            assert!(expected.verify(&[polygon_item(&points)]).is_ok());
            let mut changed = points.clone();
            changed[1].0 += 0.1;
            assert!(expected.verify(&[polygon_item(&changed)]).is_err());
        }
    }

    #[test]
    fn polygon_point_dedup_preserves_arc_nodes_and_open_loops() {
        let arc = common::PolyLineNode {
            geometry: Some(common::poly_line_node::Geometry::Arc(
                common::ArcStartMidEnd {
                    start: Some(crate::builders::vec2(0.0, 0.0)),
                    mid: Some(crate::builders::vec2(1.0, 1.0)),
                    end: Some(crate::builders::vec2(2.0, 0.0)),
                },
            )),
        };
        let mut arcs = common::PolyLine {
            nodes: vec![arc.clone(), arc],
            closed: true,
        };
        normalize_polyline(&mut arcs);
        assert_eq!(arcs.nodes.len(), 2);
        let point = common::PolyLineNode {
            geometry: Some(common::poly_line_node::Geometry::Point(
                crate::builders::vec2(0.0, 0.0),
            )),
        };
        let mut open = common::PolyLine {
            nodes: vec![point.clone(), point],
            closed: false,
        };
        let wanted = open.clone();
        normalize_polyline(&mut open);
        assert_eq!(open, wanted);
    }

    #[test]
    fn presentation_accepts_native_arc_endpoint_order_but_checks_the_curve() {
        let arc_item = |start: (f64, f64), mid: (f64, f64), end: (f64, f64)| {
            let mut footprint = board::FootprintInstance::decode(item().value.as_slice()).unwrap();
            let graphic = crate::builders::board_arc(
                "F.CrtYd", 0.1, start.0, start.1, mid.0, mid.1, end.0, end.1,
            );
            footprint.definition.as_mut().unwrap().items = vec![crate::builders::pack_any(
                &graphic,
                "kiapi.board.types.BoardGraphicShape",
            )];
            crate::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
        };
        let wanted = arc_item((1.0, 0.0), (0.0, -1.0), (-1.0, 0.0));
        let expected = FootprintPresentation::from_items([&wanted]).unwrap();
        for (start, mid, end, equal) in [
            ((-1.0, 0.0), (0.0, -1.0), (1.0, 0.0), true),
            ((-1.0, 0.0), (0.0, 1.0), (1.0, 0.0), false),
            ((-1.1, 0.0), (0.0, -1.0), (1.0, 0.0), false),
            ((-1.0, 0.0), (0.0, -1.1), (1.0, 0.0), false),
        ] {
            assert_eq!(expected.verify(&[arc_item(start, mid, end)]).is_ok(), equal);
        }
    }

    #[test]
    fn presentation_refuses_missing_ambiguous_malformed_or_replaced_targets() {
        let wanted = item();
        let expected = FootprintPresentation::from_items([&wanted]).unwrap();
        assert!(expected.verify(&[]).is_err());
        assert!(expected.verify(&[wanted.clone(), wanted.clone()]).is_err());
        let mut malformed = wanted.clone();
        malformed.value = vec![0xff];
        assert!(expected.verify(&[malformed]).is_err());
        for change in 0..5 {
            let mut footprint = board::FootprintInstance::decode(wanted.value.as_slice()).unwrap();
            match change {
                0 => footprint.id.as_mut().unwrap().value = "different-footprint".into(),
                1 => footprint.position.as_mut().unwrap().x_nm += 1,
                2 => footprint.layer = board::BoardLayer::BlBCu as i32,
                3 => footprint.definition.as_mut().unwrap().items[0].value = vec![0xff],
                _ => {
                    footprint
                        .reference_field
                        .as_mut()
                        .unwrap()
                        .text
                        .as_mut()
                        .unwrap()
                        .parent = Some(common::Kiid {
                        value: "foreign-parent".into(),
                    })
                }
            }
            assert!(expected
                .verify(&[crate::builders::pack_any(
                    &footprint,
                    "kiapi.board.types.FootprintInstance"
                )])
                .is_err());
        }
    }

    #[test]
    fn presentation_ignores_assigned_child_ids_parent_ids_order_and_full_turns() {
        let wanted = item();
        let expected = FootprintPresentation::from_items([&wanted]).unwrap();
        let mut actual = board::FootprintInstance::decode(wanted.value.as_slice()).unwrap();
        let capture = board::FootprintInstance::decode(
            include_bytes!("../../konnect-core/tests/fixtures/issue_474_r1.ipc.bin").as_slice(),
        )
        .unwrap();
        let reference = actual.reference_field.as_mut().unwrap();
        reference.id = capture.reference_field.unwrap().id;
        reference.text.as_mut().unwrap().id = Some(common::Kiid {
            value: "assigned-text-id".into(),
        });
        reference.text.as_mut().unwrap().parent = Some(common::Kiid {
            value: "target-footprint".into(),
        });
        reference
            .text
            .as_mut()
            .unwrap()
            .text
            .as_mut()
            .unwrap()
            .attributes
            .as_mut()
            .unwrap()
            .angle = Some(common::Angle {
            value_degrees: 360.0,
        });
        actual.definition.as_mut().unwrap().items.reverse();
        assert!(expected
            .verify(&[crate::builders::pack_any(
                &actual,
                "kiapi.board.types.FootprintInstance"
            )])
            .is_ok());
    }
}
