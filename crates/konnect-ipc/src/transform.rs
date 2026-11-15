//! Rigid-body transforms for `FootprintInstance` round-trips (issue #23).
//!
//! KiCAD 10 serializes every child of a footprint (pads, fields, texts,
//! graphics, zones) in ABSOLUTE board coordinates — despite the proto comment
//! on `Pad.position` claiming footprint-relative coordinates — and
//! `FOOTPRINT::Deserialize` clears all children and re-creates them at exactly
//! the coordinates carried in the message. Round-tripping an instance with
//! only `position` or `orientation` changed therefore leaves the copper and
//! silkscreen at the old location, detached from the anchor. Before sending
//! `UpdateItems`, every child must get the same rigid-body transform KiCAD
//! would apply natively (`FOOTPRINT::SetPosition` / `SetOrientation`).

use crate::gen::kiapi;
use anyhow::Result;
use kiapi::board::types as board;
use kiapi::common::types as common;
use prost::Message;

/// The transform to apply to a footprint's children.
#[derive(Clone, Copy)]
pub enum Xform {
    /// Shift every child by (dx, dy) nanometers.
    Translate { dx_nm: i64, dy_nm: i64 },
    /// Rotate every child by `delta_deg` around (cx, cy), matching KiCAD's
    /// convention (Y axis down; positive angles counterclockwise on screen).
    Rotate {
        cx_nm: i64,
        cy_nm: i64,
        delta_deg: f64,
    },
}

impl Xform {
    pub(crate) fn point(&self, x: i64, y: i64) -> Result<(i64, i64)> {
        let add = |a: i64, b: i64| {
            a.checked_add(b).ok_or_else(|| {
                anyhow::anyhow!("transformed coordinate exceeds the nanometre range")
            })
        };
        match *self {
            Xform::Translate { dx_nm, dy_nm } => Ok((add(x, dx_nm)?, add(y, dy_nm)?)),
            Xform::Rotate {
                cx_nm,
                cy_nm,
                delta_deg,
            } => {
                let difference = |a: i64, b: i64| {
                    a.checked_sub(b).ok_or_else(|| {
                        anyhow::anyhow!("rotation offset exceeds the nanometre range")
                    })
                };
                let (rx, ry) = (difference(x, cx_nm)? as f64, difference(y, cy_nm)? as f64);
                let (s, c) = delta_deg.to_radians().sin_cos();
                Ok((
                    add(cx_nm, crate::builders::round_nanometres(rx * c + ry * s)?)?,
                    add(cy_nm, crate::builders::round_nanometres(ry * c - rx * s)?)?,
                ))
            }
        }
    }

    fn angle_delta_deg(&self) -> f64 {
        match *self {
            Xform::Translate { .. } => 0.0,
            Xform::Rotate { delta_deg, .. } => delta_deg,
        }
    }

    /// True when the rotation keeps axis-aligned shapes axis-aligned.
    fn is_cardinal(&self) -> bool {
        let r = self.angle_delta_deg().rem_euclid(90.0);
        r.abs() < 1e-9 || (90.0 - r).abs() < 1e-9
    }
}

/// Normalize to (-180, 180], the range KiCAD's `EDA_ANGLE::Normalize180` uses.
fn normalize_deg(deg: f64) -> f64 {
    let mut d = deg % 360.0;
    if d <= -180.0 {
        d += 360.0;
    } else if d > 180.0 {
        d -= 360.0;
    }
    d
}

fn xform_vec2(v: &mut Option<common::Vector2>, xf: &Xform) -> Result<()> {
    if let Some(p) = v.as_mut() {
        let (x, y) = xf.point(p.x_nm, p.y_nm)?;
        p.x_nm = x;
        p.y_nm = y;
    }
    Ok(())
}

fn xform_angle(a: &mut Option<common::Angle>, xf: &Xform) {
    let delta = xf.angle_delta_deg();
    if delta != 0.0 {
        let base = a.as_ref().map(|x| x.value_degrees).unwrap_or(0.0);
        *a = Some(common::Angle {
            value_degrees: normalize_deg(base + delta),
        });
    }
}

fn xform_text(t: &mut Option<common::Text>, xf: &Xform) -> Result<()> {
    if let Some(text) = t.as_mut() {
        xform_vec2(&mut text.position, xf)?;
        if let Some(attrs) = text.attributes.as_mut() {
            xform_angle(&mut attrs.angle, xf);
        }
    }
    Ok(())
}

fn xform_board_text(bt: &mut board::BoardText, xf: &Xform) -> Result<()> {
    xform_text(&mut bt.text, xf)?;
    Ok(())
}

fn xform_field(f: &mut Option<board::Field>, xf: &Xform) -> Result<()> {
    if let Some(field) = f.as_mut() {
        if let Some(text) = field.text.as_mut() {
            xform_board_text(text, xf)?;
        }
    }
    Ok(())
}

fn xform_polyline(pl: &mut common::PolyLine, xf: &Xform) -> Result<()> {
    for node in pl.nodes.iter_mut() {
        match node.geometry.as_mut() {
            Some(common::poly_line_node::Geometry::Point(p)) => {
                let (x, y) = xf.point(p.x_nm, p.y_nm)?;
                p.x_nm = x;
                p.y_nm = y;
            }
            Some(common::poly_line_node::Geometry::Arc(arc)) => {
                xform_vec2(&mut arc.start, xf)?;
                xform_vec2(&mut arc.mid, xf)?;
                xform_vec2(&mut arc.end, xf)?;
            }
            None => {}
        }
    }
    Ok(())
}

fn xform_polyset(ps: &mut common::PolySet, xf: &Xform) -> Result<()> {
    for poly in ps.polygons.iter_mut() {
        if let Some(outline) = poly.outline.as_mut() {
            xform_polyline(outline, xf)?;
        }
        for hole in poly.holes.iter_mut() {
            xform_polyline(hole, xf)?;
        }
    }
    Ok(())
}

fn xform_graphic_shape(gs: &mut board::BoardGraphicShape, xf: &Xform) -> Result<()> {
    use common::graphic_shape::Geometry;

    let Some(shape) = gs.shape.as_mut() else {
        return Ok(());
    };
    match shape.geometry.as_mut() {
        Some(Geometry::Segment(seg)) => {
            xform_vec2(&mut seg.start, xf)?;
            xform_vec2(&mut seg.end, xf)?;
        }
        Some(Geometry::Rectangle(rect)) => {
            if xf.is_cardinal() {
                xform_vec2(&mut rect.top_left, xf)?;
                xform_vec2(&mut rect.bottom_right, xf)?;
            } else if rect.corner_radius.is_some() {
                // A rounded rectangle can't be represented off-axis; KiCAD
                // itself converts plain rectangles to polygons here, but has
                // no lossless form for rounded ones.
                anyhow::bail!(
                    "cannot rotate a footprint containing a rounded-rectangle \
                     graphic by a non-90° angle; use a multiple of 90°"
                );
            } else {
                // Mirror KiCAD's EDA_SHAPE::Rotate: an off-axis rectangle
                // becomes a closed polygon of its four rotated corners.
                let (tl, br) = (
                    rect.top_left.unwrap_or_default(),
                    rect.bottom_right.unwrap_or_default(),
                );
                let corners = [
                    (tl.x_nm, tl.y_nm),
                    (br.x_nm, tl.y_nm),
                    (br.x_nm, br.y_nm),
                    (tl.x_nm, br.y_nm),
                ];
                let nodes = corners
                    .iter()
                    .map(|&(x, y)| {
                        let (nx, ny) = xf.point(x, y)?;
                        Ok(common::PolyLineNode {
                            geometry: Some(common::poly_line_node::Geometry::Point(
                                common::Vector2 { x_nm: nx, y_nm: ny },
                            )),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                shape.geometry = Some(Geometry::Polygon(common::PolySet {
                    polygons: vec![common::PolygonWithHoles {
                        outline: Some(common::PolyLine {
                            nodes,
                            closed: true,
                        }),
                        holes: vec![],
                    }],
                }));
            }
        }
        Some(Geometry::Arc(arc)) => {
            xform_vec2(&mut arc.start, xf)?;
            xform_vec2(&mut arc.mid, xf)?;
            xform_vec2(&mut arc.end, xf)?;
        }
        Some(Geometry::Circle(circle)) => {
            xform_vec2(&mut circle.center, xf)?;
            xform_vec2(&mut circle.radius_point, xf)?;
        }
        Some(Geometry::Polygon(polyset)) => {
            xform_polyset(polyset, xf)?;
        }
        Some(Geometry::Bezier(bez)) => {
            xform_vec2(&mut bez.start, xf)?;
            xform_vec2(&mut bez.control1, xf)?;
            xform_vec2(&mut bez.control2, xf)?;
            xform_vec2(&mut bez.end, xf)?;
        }
        None => {}
    }
    Ok(())
}

fn xform_zone(zone: &mut board::Zone, xf: &Xform) -> Result<()> {
    if let Some(outline) = zone.outline.as_mut() {
        xform_polyset(outline, xf)?;
    }
    for filled in zone.filled_polygons.iter_mut() {
        if let Some(shapes) = filled.shapes.as_mut() {
            xform_polyset(shapes, xf)?;
        }
    }
    Ok(())
}

fn xform_dimension(dim: &mut board::Dimension, xf: &Xform) -> Result<()> {
    use board::dimension::DimensionStyle;

    xform_text(&mut dim.text, xf)?;
    match dim.dimension_style.as_mut() {
        Some(DimensionStyle::Aligned(a)) => {
            xform_vec2(&mut a.start, xf)?;
            xform_vec2(&mut a.end, xf)?;
        }
        Some(DimensionStyle::Orthogonal(o)) => {
            xform_vec2(&mut o.start, xf)?;
            xform_vec2(&mut o.end, xf)?;
        }
        Some(DimensionStyle::Radial(r)) => {
            xform_vec2(&mut r.center, xf)?;
            xform_vec2(&mut r.radius_point, xf)?;
        }
        Some(DimensionStyle::Leader(l)) => {
            xform_vec2(&mut l.start, xf)?;
            xform_vec2(&mut l.end, xf)?;
        }
        Some(DimensionStyle::Center(c)) => {
            xform_vec2(&mut c.center, xf)?;
            xform_vec2(&mut c.end, xf)?;
        }
        None => {}
    }
    Ok(())
}

/// Apply `xf` to every position-bearing child carried in `fp`: the four
/// mandatory fields plus all `definition.items`. Does NOT touch
/// `fp.position` / `fp.orientation` — the caller sets those.
pub fn transform_footprint_children(fp: &mut board::FootprintInstance, xf: &Xform) -> Result<()> {
    if !xf.angle_delta_deg().is_finite() {
        anyhow::bail!("footprint rotation must be finite");
    }
    xform_field(&mut fp.reference_field, xf)?;
    xform_field(&mut fp.value_field, xf)?;
    xform_field(&mut fp.datasheet_field, xf)?;
    xform_field(&mut fp.description_field, xf)?;

    let Some(def) = fp.definition.as_mut() else {
        return Ok(());
    };

    for item in def.items.iter_mut() {
        // Discriminate on the declared type, never on whether a decode
        // happens to succeed — see `builders::any_is` for why that distinction
        // cost every synced footprint its graphics (#244).
        let url = crate::builders::any_type_name(item);
        if url == "kiapi.board.types.Pad" {
            let mut pad = board::Pad::decode(item.value.as_slice())?;
            xform_vec2(&mut pad.position, xf)?;
            if let Some(ps) = pad.pad_stack.as_mut() {
                xform_angle(&mut ps.angle, xf);
            }
            item.value = pad.encode_to_vec();
        } else if url == "kiapi.board.types.BoardText" {
            let mut text = board::BoardText::decode(item.value.as_slice())?;
            xform_board_text(&mut text, xf)?;
            item.value = text.encode_to_vec();
        } else if url == "kiapi.board.types.BoardTextBox" {
            if !xf.is_cardinal() {
                anyhow::bail!(
                    "cannot rotate a footprint containing a textbox by a \
                     non-90° angle; use a multiple of 90°"
                );
            }
            let mut tb = board::BoardTextBox::decode(item.value.as_slice())?;
            if let Some(textbox) = tb.textbox.as_mut() {
                xform_vec2(&mut textbox.top_left, xf)?;
                xform_vec2(&mut textbox.bottom_right, xf)?;
                if let Some(attrs) = textbox.attributes.as_mut() {
                    xform_angle(&mut attrs.angle, xf);
                }
            }
            item.value = tb.encode_to_vec();
        } else if url == "kiapi.board.types.Field" {
            let mut field = board::Field::decode(item.value.as_slice())?;
            if let Some(text) = field.text.as_mut() {
                xform_board_text(text, xf)?;
            }
            item.value = field.encode_to_vec();
        } else if url == "kiapi.board.types.BoardGraphicShape" {
            let mut shape = board::BoardGraphicShape::decode(item.value.as_slice())?;
            xform_graphic_shape(&mut shape, xf)?;
            item.value = shape.encode_to_vec();
        } else if url == "kiapi.board.types.Zone" {
            let mut zone = board::Zone::decode(item.value.as_slice())?;
            xform_zone(&mut zone, xf)?;
            item.value = zone.encode_to_vec();
        } else if url == "kiapi.board.types.Dimension" {
            let mut dim = board::Dimension::decode(item.value.as_slice())?;
            xform_dimension(&mut dim, xf)?;
            item.value = dim.encode_to_vec();
        } else if url == "kiapi.board.types.Footprint3DModel" || url == "kiapi.board.types.Group" {
            // No board-space geometry of their own — nothing to shift.
        } else {
            // KiCAD would re-create this item wherever the stale message
            // says it is; refusing beats silently corrupting the board.
            anyhow::bail!(
                "footprint contains an item of unsupported type '{url}'; \
                 refusing to move/rotate it via IPC to avoid detaching it"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builders;

    #[test]
    fn coordinate_transforms_refuse_overflow() {
        assert!(Xform::Translate { dx_nm: 1, dy_nm: 0 }
            .point(i64::MAX, 0)
            .is_err());
        assert!(Xform::Rotate {
            cx_nm: i64::MIN,
            cy_nm: 0,
            delta_deg: 90.0
        }
        .point(i64::MAX, 0)
        .is_err());
        assert!(Xform::Rotate {
            cx_nm: 0,
            cy_nm: 0,
            delta_deg: f64::INFINITY
        }
        .point(1, 1)
        .is_err());
    }

    fn pad_at(x_mm: f64, y_mm: f64, angle_deg: f64) -> board::Pad {
        board::Pad {
            position: Some(builders::vec2(x_mm, y_mm).unwrap()),
            pad_stack: Some(board::PadStack {
                angle: Some(common::Angle {
                    value_degrees: angle_deg,
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn field_at(x_mm: f64, y_mm: f64) -> board::Field {
        board::Field {
            text: Some(board::BoardText {
                text: Some(common::Text {
                    position: Some(builders::vec2(x_mm, y_mm).unwrap()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn instance_with(items: Vec<prost_types::Any>) -> board::FootprintInstance {
        board::FootprintInstance {
            position: Some(builders::vec2(100.0, 100.0).unwrap()),
            reference_field: Some(field_at(100.0, 98.0)),
            definition: Some(board::Footprint {
                items,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn decode_pad(any: &prost_types::Any) -> board::Pad {
        board::Pad::decode(any.value.as_slice()).unwrap()
    }

    #[test]
    fn translate_shifts_pads_fields_and_graphics() {
        let pad = builders::pack_any(&pad_at(100.0, 100.0, 0.0), "kiapi.board.types.Pad");
        let seg = builders::pack_any(
            &builders::board_segment("F.SilkS", 0.1, 99.0, 99.0, 101.0, 99.0).unwrap(),
            "kiapi.board.types.BoardGraphicShape",
        );
        let mut fp = instance_with(vec![pad, seg]);

        // Move 100,100 -> 50,50: delta is -50mm on both axes.
        let xf = Xform::Translate {
            dx_nm: builders::mm_to_nm(-50.0).unwrap(),
            dy_nm: builders::mm_to_nm(-50.0).unwrap(),
        };
        transform_footprint_children(&mut fp, &xf).unwrap();

        let items = &fp.definition.as_ref().unwrap().items;
        let pad = decode_pad(&items[0]);
        assert_eq!(pad.position.unwrap(), builders::vec2(50.0, 50.0).unwrap());

        let shape = board::BoardGraphicShape::decode(items[1].value.as_slice()).unwrap();
        match shape.shape.unwrap().geometry.unwrap() {
            common::graphic_shape::Geometry::Segment(s) => {
                assert_eq!(s.start.unwrap(), builders::vec2(49.0, 49.0).unwrap());
                assert_eq!(s.end.unwrap(), builders::vec2(51.0, 49.0).unwrap());
            }
            other => panic!("expected segment, got {other:?}"),
        }

        let ref_text = fp.reference_field.unwrap().text.unwrap().text.unwrap();
        assert_eq!(
            ref_text.position.unwrap(),
            builders::vec2(50.0, 48.0).unwrap()
        );
    }

    #[test]
    fn rotate_90_matches_kicad_convention() {
        // Pad 1mm to the right of the anchor, at pad angle 0.
        let pad = builders::pack_any(&pad_at(101.0, 100.0, 0.0), "kiapi.board.types.Pad");
        let mut fp = instance_with(vec![pad]);

        let xf = Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0).unwrap(),
            cy_nm: builders::mm_to_nm(100.0).unwrap(),
            delta_deg: 90.0,
        };
        transform_footprint_children(&mut fp, &xf).unwrap();

        // KiCAD positive rotation is counterclockwise on screen (Y down):
        // (+1, 0) relative -> (0, -1) relative.
        let pad = decode_pad(&fp.definition.as_ref().unwrap().items[0]);
        assert_eq!(pad.position.unwrap(), builders::vec2(100.0, 99.0).unwrap());
        assert_eq!(pad.pad_stack.unwrap().angle.unwrap().value_degrees, 90.0);
    }

    #[test]
    fn rotate_angle_normalizes_to_plus_minus_180() {
        let pad = builders::pack_any(&pad_at(100.0, 100.0, 170.0), "kiapi.board.types.Pad");
        let mut fp = instance_with(vec![pad]);

        let xf = Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0).unwrap(),
            cy_nm: builders::mm_to_nm(100.0).unwrap(),
            delta_deg: 20.0,
        };
        transform_footprint_children(&mut fp, &xf).unwrap();

        let pad = decode_pad(&fp.definition.as_ref().unwrap().items[0]);
        assert_eq!(pad.pad_stack.unwrap().angle.unwrap().value_degrees, -170.0);
    }

    #[test]
    fn non_cardinal_rotation_turns_rectangle_into_polygon() {
        let rect = builders::pack_any(
            &builders::board_rectangle("F.Fab", 0.1, 99.0, 99.0, 101.0, 101.0, false).unwrap(),
            "kiapi.board.types.BoardGraphicShape",
        );
        let mut fp = instance_with(vec![rect]);

        let xf = Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0).unwrap(),
            cy_nm: builders::mm_to_nm(100.0).unwrap(),
            delta_deg: 45.0,
        };
        transform_footprint_children(&mut fp, &xf).unwrap();

        let shape = board::BoardGraphicShape::decode(
            fp.definition.as_ref().unwrap().items[0].value.as_slice(),
        )
        .unwrap();
        match shape.shape.unwrap().geometry.unwrap() {
            common::graphic_shape::Geometry::Polygon(ps) => {
                let outline = ps.polygons[0].outline.as_ref().unwrap();
                assert!(outline.closed);
                assert_eq!(outline.nodes.len(), 4);
            }
            other => panic!("expected polygon after 45° rotation, got {other:?}"),
        }
    }

    #[test]
    fn cardinal_rotation_keeps_rectangle_a_rectangle() {
        let rect = builders::pack_any(
            &builders::board_rectangle("F.Fab", 0.1, 99.0, 99.0, 101.0, 101.0, false).unwrap(),
            "kiapi.board.types.BoardGraphicShape",
        );
        let mut fp = instance_with(vec![rect]);

        let xf = Xform::Rotate {
            cx_nm: builders::mm_to_nm(100.0).unwrap(),
            cy_nm: builders::mm_to_nm(100.0).unwrap(),
            delta_deg: 180.0,
        };
        transform_footprint_children(&mut fp, &xf).unwrap();

        let shape = board::BoardGraphicShape::decode(
            fp.definition.as_ref().unwrap().items[0].value.as_slice(),
        )
        .unwrap();
        assert!(matches!(
            shape.shape.unwrap().geometry.unwrap(),
            common::graphic_shape::Geometry::Rectangle(_)
        ));
    }

    #[test]
    fn unknown_item_type_is_rejected_not_corrupted() {
        let bogus = prost_types::Any {
            type_url: "type.googleapis.com/kiapi.board.types.SomethingNew".to_string(),
            value: vec![],
        };
        let mut fp = instance_with(vec![bogus]);

        let xf = Xform::Translate {
            dx_nm: 1000,
            dy_nm: 0,
        };
        let err = transform_footprint_children(&mut fp, &xf).unwrap_err();
        assert!(err.to_string().contains("SomethingNew"));
    }

    #[test]
    fn model_and_group_items_pass_through_untouched() {
        let model = builders::pack_any(
            &board::Footprint3DModel::default(),
            "kiapi.board.types.Footprint3DModel",
        );
        let original = model.value.clone();
        let mut fp = instance_with(vec![model]);

        let xf = Xform::Translate {
            dx_nm: builders::mm_to_nm(10.0).unwrap(),
            dy_nm: 0,
        };
        transform_footprint_children(&mut fp, &xf).unwrap();
        assert_eq!(fp.definition.unwrap().items[0].value, original);
    }

    #[test]
    fn zone_outlines_holes_and_filled_arcs_move_with_the_footprint() {
        use common::poly_line_node::Geometry;
        let point = |x, y| common::Vector2 { x_nm: x, y_nm: y };
        let outline = common::PolyLine {
            closed: true,
            nodes: vec![common::PolyLineNode {
                geometry: Some(Geometry::Point(point(10, 20))),
            }],
        };
        let hole = common::PolyLine {
            closed: true,
            nodes: vec![common::PolyLineNode {
                geometry: Some(Geometry::Arc(common::ArcStartMidEnd {
                    start: Some(point(1, 2)),
                    mid: Some(point(3, 4)),
                    end: Some(point(5, 6)),
                })),
            }],
        };
        let shapes = common::PolySet {
            polygons: vec![common::PolygonWithHoles {
                outline: Some(outline),
                holes: vec![hole],
            }],
        };
        let zone = board::Zone {
            outline: Some(shapes.clone()),
            filled: true,
            filled_polygons: vec![board::ZoneFilledPolygons {
                layer: board::BoardLayer::BlFCu as i32,
                shapes: Some(shapes),
            }],
            ..Default::default()
        };
        let mut fp = instance_with(vec![builders::pack_any(&zone, "kiapi.board.types.Zone")]);
        let anchor = fp.position;
        transform_footprint_children(
            &mut fp,
            &Xform::Translate {
                dx_nm: 100,
                dy_nm: -50,
            },
        )
        .unwrap();
        assert_eq!(fp.position, anchor);
        let zone = board::Zone::decode(fp.definition.unwrap().items[0].value.as_slice()).unwrap();
        assert!(zone.filled);
        for shapes in [
            zone.outline.unwrap(),
            zone.filled_polygons[0].shapes.clone().unwrap(),
        ] {
            let polygon = &shapes.polygons[0];
            assert_eq!(
                polygon.outline.as_ref().unwrap().nodes[0].geometry,
                Some(Geometry::Point(point(110, -30)))
            );
            assert_eq!(
                polygon.holes[0].nodes[0].geometry,
                Some(Geometry::Arc(common::ArcStartMidEnd {
                    start: Some(point(101, -48)),
                    mid: Some(point(103, -46)),
                    end: Some(point(105, -44)),
                }))
            );
            assert!(polygon.holes[0].closed);
        }
    }

    #[test]
    fn each_dimension_style_keeps_its_endpoints_attached_on_rotation() {
        use board::dimension::DimensionStyle;
        let start = Some(common::Vector2 { x_nm: 10, y_nm: 20 });
        let end = Some(common::Vector2 { x_nm: 30, y_nm: 40 });
        let styles = [
            DimensionStyle::Aligned(board::AlignedDimensionAttributes {
                start,
                end,
                ..Default::default()
            }),
            DimensionStyle::Orthogonal(board::OrthogonalDimensionAttributes {
                start,
                end,
                ..Default::default()
            }),
            DimensionStyle::Radial(board::RadialDimensionAttributes {
                center: start,
                radius_point: end,
                ..Default::default()
            }),
            DimensionStyle::Leader(board::LeaderDimensionAttributes {
                start,
                end,
                ..Default::default()
            }),
            DimensionStyle::Center(board::CenterDimensionAttributes { center: start, end }),
        ];
        for style in styles {
            let dimension = board::Dimension {
                dimension_style: Some(style),
                text: Some(common::Text {
                    text: "measured".into(),
                    position: start,
                    attributes: Some(common::TextAttributes::default()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let mut fp = instance_with(vec![builders::pack_any(
                &dimension,
                "kiapi.board.types.Dimension",
            )]);
            transform_footprint_children(
                &mut fp,
                &Xform::Rotate {
                    cx_nm: 0,
                    cy_nm: 0,
                    delta_deg: 90.0,
                },
            )
            .unwrap();
            let dimension =
                board::Dimension::decode(fp.definition.unwrap().items[0].value.as_slice()).unwrap();
            let endpoints = match dimension.dimension_style.unwrap() {
                DimensionStyle::Aligned(v) => (v.start, v.end),
                DimensionStyle::Orthogonal(v) => (v.start, v.end),
                DimensionStyle::Radial(v) => (v.center, v.radius_point),
                DimensionStyle::Leader(v) => (v.start, v.end),
                DimensionStyle::Center(v) => (v.center, v.end),
            };
            assert_eq!(
                endpoints,
                (
                    Some(common::Vector2 {
                        x_nm: 20,
                        y_nm: -10
                    }),
                    Some(common::Vector2 {
                        x_nm: 40,
                        y_nm: -30
                    }),
                )
            );
            let text = dimension.text.unwrap();
            assert_eq!(text.text, "measured");
            assert_eq!(text.position, endpoints.0);
            assert_eq!(text.attributes.unwrap().angle.unwrap().value_degrees, 90.0);
        }
    }

    #[test]
    fn textbox_and_custom_field_follow_cardinal_rotation() {
        let position = Some(common::Vector2 { x_nm: 10, y_nm: 20 });
        let textbox = board::BoardTextBox {
            textbox: Some(common::TextBox {
                top_left: position,
                bottom_right: Some(common::Vector2 { x_nm: 30, y_nm: 40 }),
                attributes: Some(common::TextAttributes::default()),
                text: "assembly".into(),
            }),
            ..Default::default()
        };
        let field = board::Field {
            name: "MPN".into(),
            text: Some(board::BoardText {
                text: Some(common::Text {
                    text: "part".into(),
                    position,
                    attributes: Some(common::TextAttributes::default()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut fp = instance_with(vec![
            builders::pack_any(&textbox, "kiapi.board.types.BoardTextBox"),
            builders::pack_any(&field, "kiapi.board.types.Field"),
        ]);
        transform_footprint_children(
            &mut fp,
            &Xform::Rotate {
                cx_nm: 0,
                cy_nm: 0,
                delta_deg: -90.0,
            },
        )
        .unwrap();
        let items = fp.definition.unwrap().items;
        let textbox = board::BoardTextBox::decode(items[0].value.as_slice())
            .unwrap()
            .textbox
            .unwrap();
        assert_eq!(
            textbox.top_left,
            Some(common::Vector2 {
                x_nm: -20,
                y_nm: 10
            })
        );
        assert_eq!(
            textbox.bottom_right,
            Some(common::Vector2 {
                x_nm: -40,
                y_nm: 30
            })
        );
        assert_eq!(
            textbox.attributes.unwrap().angle.unwrap().value_degrees,
            -90.0
        );
        let field = board::Field::decode(items[1].value.as_slice()).unwrap();
        assert_eq!(field.name, "MPN");
        let text = field.text.unwrap().text.unwrap();
        assert_eq!(text.text, "part");
        assert_eq!(
            text.position,
            Some(common::Vector2 {
                x_nm: -20,
                y_nm: 10
            })
        );
    }

    #[test]
    fn lossy_rotations_and_malformed_typed_children_are_refused() {
        let mut rectangle =
            builders::board_rectangle("F.Fab", 0.1, 0.0, 0.0, 1.0, 1.0, false).unwrap();
        if let Some(common::graphic_shape::Geometry::Rectangle(rect)) =
            rectangle.shape.as_mut().unwrap().geometry.as_mut()
        {
            rect.corner_radius = Some(common::Distance { value_nm: 100 });
        }
        let noncardinal = Xform::Rotate {
            cx_nm: 0,
            cy_nm: 0,
            delta_deg: 45.0,
        };
        for (item, reason) in [
            (
                builders::pack_any(&rectangle, "kiapi.board.types.BoardGraphicShape"),
                "rounded-rectangle",
            ),
            (
                builders::pack_any(
                    &board::BoardTextBox::default(),
                    "kiapi.board.types.BoardTextBox",
                ),
                "textbox",
            ),
        ] {
            let mut fp = instance_with(vec![item]);
            let before = fp.definition.clone();
            let error = transform_footprint_children(&mut fp, &noncardinal).unwrap_err();
            assert!(error.to_string().contains(reason));
            assert_eq!(fp.definition, before, "refused child must remain unchanged");
        }
        for kind in [
            "Pad",
            "BoardText",
            "BoardTextBox",
            "Field",
            "BoardGraphicShape",
            "Zone",
            "Dimension",
        ] {
            let mut fp = instance_with(vec![prost_types::Any {
                type_url: format!("type.googleapis.com/kiapi.board.types.{kind}"),
                value: vec![0xff],
            }]);
            assert!(
                transform_footprint_children(&mut fp, &Xform::Translate { dx_nm: 1, dy_nm: 2 })
                    .is_err(),
                "{kind}"
            );
        }
    }

    #[test]
    fn omitted_optional_child_geometry_does_not_create_new_objects() {
        let items = vec![
            builders::pack_any(&board::Pad::default(), "kiapi.board.types.Pad"),
            builders::pack_any(&board::BoardText::default(), "kiapi.board.types.BoardText"),
            builders::pack_any(
                &board::BoardTextBox::default(),
                "kiapi.board.types.BoardTextBox",
            ),
            builders::pack_any(&board::Field::default(), "kiapi.board.types.Field"),
            builders::pack_any(
                &board::BoardGraphicShape::default(),
                "kiapi.board.types.BoardGraphicShape",
            ),
            builders::pack_any(&board::Zone::default(), "kiapi.board.types.Zone"),
            builders::pack_any(&board::Dimension::default(), "kiapi.board.types.Dimension"),
            builders::pack_any(&board::Group::default(), "kiapi.board.types.Group"),
        ];
        let mut fp = board::FootprintInstance {
            definition: Some(board::Footprint {
                items: items.clone(),
                ..Default::default()
            }),
            ..Default::default()
        };
        transform_footprint_children(
            &mut fp,
            &Xform::Translate {
                dx_nm: 10,
                dy_nm: 20,
            },
        )
        .unwrap();
        assert_eq!(fp.definition.unwrap().items, items);
        let mut no_definition = board::FootprintInstance::default();
        transform_footprint_children(
            &mut no_definition,
            &Xform::Translate {
                dx_nm: 10,
                dy_nm: 20,
            },
        )
        .unwrap();
        assert_eq!(no_definition, board::FootprintInstance::default());
    }
}
