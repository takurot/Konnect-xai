//! Read-only layout collisions on a single schematic sheet.

use konnect_sexp::{
    geometry::{points_coincident, round6, transform_pin, PinTransform},
    schematic::{
        extract_lib_pins_for_unit, extract_symbol_instance, extract_wires, find_lib_symbol,
        parse_at, pin_endpoint, symbol_body_bounds_for_instance, SymbolBounds, SymbolInstance,
    },
    SexpNode,
};
use serde_json::{json, Value};

const EPSILON: f64 = 1e-9;
type Point = (f64, f64);

#[derive(Clone, Copy)]
struct Segment {
    start: Point,
    end: Point,
}

impl Segment {
    fn direction(self) -> Point {
        (self.end.0 - self.start.0, self.end.1 - self.start.1)
    }

    /// Clip against the open rectangle so a boundary touch stays clear.
    fn crosses_interior(self, bounds: SymbolBounds) -> bool {
        let delta = self.direction();
        if ![self.start.0, self.start.1, delta.0, delta.1]
            .into_iter()
            .all(f64::is_finite)
            || delta.0.abs().max(delta.1.abs()) <= EPSILON
        {
            return false;
        }
        let (mut entry, mut exit) = (0_f64, 1_f64);
        for (start, delta, min, max) in [
            (self.start.0, delta.0, bounds.min_x, bounds.max_x),
            (self.start.1, delta.1, bounds.min_y, bounds.max_y),
        ] {
            let (min, max) = (min + EPSILON, max - EPSILON);
            if min >= max {
                return false;
            }
            if delta.abs() <= EPSILON {
                if start <= min || start >= max {
                    return false;
                }
            } else {
                let t1 = (min - start) / delta;
                let t2 = (max - start) / delta;
                entry = entry.max(t1.min(t2));
                exit = exit.min(t1.max(t2));
            }
        }
        exit > entry
    }

    /// A pin starts at its electrical tip. Tip contacts are legal, while
    /// a crossing beyond both tips or a shared length is a collision.
    fn overlaps_pin(self, other: Self) -> bool {
        let a = self.direction();
        let b = other.direction();
        if a.0.abs().max(a.1.abs()).min(b.0.abs().max(b.1.abs())) <= EPSILON {
            return false;
        }
        let offset = (other.start.0 - self.start.0, other.start.1 - self.start.1);
        let cross = |(ax, ay): Point, (bx, by): Point| ax * by - ay * bx;
        let det = cross(a, b);
        if det.abs() > EPSILON {
            let t = cross(offset, b) / det;
            let u = cross(offset, a) / det;
            return t > EPSILON && u > EPSILON && t <= 1.0 + EPSILON && u <= 1.0 + EPSILON;
        }
        if cross(offset, a).abs() > EPSILON {
            return false;
        }
        let coord = |p: Point| if a.0.abs() >= a.1.abs() { p.0 } else { p.1 };
        let (a1, a2) = (coord(self.start), coord(self.end));
        let (b1, b2) = (coord(other.start), coord(other.end));
        a1.max(a2).min(b1.max(b2)) - a1.min(a2).max(b1.min(b2)) > EPSILON
    }
}

fn boxes_overlap(a: SymbolBounds, b: SymbolBounds) -> bool {
    let (x, y) = a.overlap_depth(b);
    x > EPSILON && y > EPSILON
}

struct PlacedSymbol<'a> {
    source: &'a SexpNode,
    instance: SymbolInstance,
    body: Option<SymbolBounds>,
    pins: Vec<Segment>,
}

impl PlacedSymbol<'_> {
    fn bounds_resolved(&self) -> bool {
        self.body.is_some() || !self.pins.is_empty()
    }

    fn pins_cross(&self, other: &Self) -> bool {
        other
            .body
            .is_some_and(|body| self.pins.iter().any(|pin| pin.crosses_interior(body)))
            || self
                .pins
                .iter()
                .any(|a| other.pins.iter().any(|b| a.overlaps_pin(*b)))
    }
}

fn placed_symbols(tree: &SexpNode) -> Vec<PlacedSymbol<'_>> {
    let lib_symbols = tree
        .find("lib_symbols")
        .map(|node| node.find_all("symbol"))
        .unwrap_or_default();
    tree.find_all("symbol")
        .into_iter()
        .filter_map(|source| {
            let instance = extract_symbol_instance(source)?;
            let lib = find_lib_symbol(&lib_symbols, &instance);
            let body = lib.and_then(|lib| symbol_body_bounds_for_instance(lib, &instance));
            let transform = instance.pin_transform();
            let pins = lib
                .map(|lib| extract_lib_pins_for_unit(lib, instance.unit))
                .unwrap_or_default()
                .into_iter()
                .map(|pin| {
                    let (sin, cos) = pin.rotation.to_radians().sin_cos();
                    Segment {
                        start: pin_endpoint(&pin, transform),
                        end: transform_pin(
                            pin.local_x + pin.length * cos,
                            pin.local_y + pin.length * sin,
                            transform,
                        ),
                    }
                })
                .filter(|segment| {
                    [segment.start, segment.end]
                        .into_iter()
                        .all(|(x, y)| x.is_finite() && y.is_finite())
                })
                .collect();
            Some(PlacedSymbol {
                source,
                instance,
                body,
                pins,
            })
        })
        .collect()
}

fn component_overlap(a: &PlacedSymbol<'_>, b: &PlacedSymbol<'_>, tolerance: f64) -> Option<Value> {
    let report = |detection| {
        json!({
            "type": "component_overlap", "detection": detection,
            "a": a.instance.reference, "unit_a": a.instance.unit, "uuid_a": a.instance.uuid,
            "b": b.instance.reference, "unit_b": b.instance.unit, "uuid_b": b.instance.uuid,
        })
    };
    if let (Some(a_body), Some(b_body)) = (a.body, b.body) {
        if boxes_overlap(a_body, b_body) {
            let (x, y) = a_body.overlap_depth(b_body);
            let bounds_json = |bounds: SymbolBounds| {
                json!({
                    "x_min": round6(bounds.min_x), "y_min": round6(bounds.min_y),
                    "x_max": round6(bounds.max_x), "y_max": round6(bounds.max_y)
                })
            };
            let mut finding = report("symbol_geometry");
            finding["overlap_x_mm"] = json!(x);
            finding["overlap_y_mm"] = json!(y);
            finding["bounds_a"] = bounds_json(a_body);
            finding["bounds_b"] = bounds_json(b_body);
            return Some(finding);
        }
    }
    if a.pins_cross(b) || b.pins_cross(a) {
        return Some(report("pin_geometry"));
    }
    if (!a.bounds_resolved() || !b.bounds_resolved())
        && points_coincident(
            a.instance.x,
            a.instance.y,
            b.instance.x,
            b.instance.y,
            tolerance,
        )
    {
        let mut finding = report("origin_fallback");
        finding["x"] = json!(round6(a.instance.x));
        finding["y"] = json!(round6(a.instance.y));
        return Some(finding);
    }
    None
}

struct TextItem<'a> {
    uuid: Option<&'a str>,
    kind: &'static str,
    text: String,
    anchor: Point,
    bounds: Option<SymbolBounds>,
    symbol: Option<&'a SexpNode>,
}

impl TextItem<'_> {
    fn is_label(&self) -> bool {
        matches!(self.kind, "label" | "global_label" | "hierarchical_label")
    }
}

fn visible_text(node: &SexpNode) -> bool {
    let has_hide = |n: &SexpNode| {
        n.find_str("hide") == Some("yes")
            || n.children().is_some_and(|children| {
                children
                    .iter()
                    .any(|child| matches!(child, SexpNode::Atom(flag) if flag == "hide"))
            })
    };
    !has_hide(node) && !node.find("effects").is_some_and(has_hide)
}

fn text_item<'a>(
    node: &'a SexpNode,
    kind: &'static str,
    owner: Option<&'a SexpNode>,
    symbol: Option<&PlacedSymbol<'a>>,
) -> Option<TextItem<'a>> {
    let is_property = owner.is_some();
    let text = node.get(if is_property { 2 } else { 1 })?.as_str()?;
    if text.trim().is_empty()
        || !visible_text(node)
        || matches!(node.get(1), Some(SexpNode::Atom(flag)) if flag == "private")
    {
        return None;
    }
    let text = if is_property && node.find_str("show_name") == Some("yes") {
        format!("{}: {text}", node.get(1)?.as_str()?)
    } else {
        text.to_owned()
    };
    let (x, y, angle) = parse_at(node)?;
    let transform = symbol.map_or(
        PinTransform {
            comp_x: x,
            comp_y: y,
            rotation_deg: 0.0,
            mirror_x: false,
            mirror_y: false,
        },
        |symbol| {
            let mut transform = symbol.instance.pin_transform();
            // Positions are saved in sheet space. The field's text angle and
            // justification are relative to the symbol's drawing transform.
            transform.comp_x = x;
            transform.comp_y = y;
            transform
        },
    );
    let bounds = text_bounds(node, kind, &text, angle, transform);
    Some(TextItem {
        uuid: node
            .find_str("uuid")
            .or_else(|| owner.and_then(|owner| owner.find_str("uuid"))),
        kind,
        text,
        anchor: (x, y),
        bounds,
        symbol: symbol.map(|placed| placed.source),
    })
}

fn text_bounds(
    node: &SexpNode,
    kind: &str,
    text: &str,
    angle: f64,
    mut transform: PinTransform,
) -> Option<SymbolBounds> {
    let effects = node.find("effects")?;
    let size = effects.find("font")?.find("size")?;
    let (font_w, font_h) = (size.get_f64(1)?, size.get_f64(2)?);
    if !font_w.is_finite() || !font_h.is_finite() || font_w <= 0.0 || font_h <= 0.0 {
        return None;
    }
    let width = text.lines().map(|line| line.chars().count()).max()? as f64 * font_w * 0.85;
    let height = font_h * (1.0 + 1.5 * (text.lines().count() - 1) as f64);
    let justify = effects.find("justify");
    let has_justify = |word| {
        justify
            .and_then(SexpNode::children)
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(word)))
    };
    let left = if has_justify("left") {
        0.0
    } else if has_justify("right") {
        -width
    } else {
        -width / 2.0
    };
    let top = if has_justify("top") {
        0.0
    } else if has_justify("bottom") {
        -height
    } else {
        -height / 2.0
    };
    let ((left, top), rotation) = match kind {
        "global_label" | "hierarchical_label" => {
            // The stored angle points from the electrical anchor into the
            // label. Leave room for the symbol outline before the glyphs.
            ((1.125 * font_h, -height / 2.0), angle)
        }
        "label" => {
            // KiCad keeps local text above a horizontal connection even
            // when the label starts to the left of the anchor.
            let flipped = angle.rem_euclid(360.0) >= 180.0;
            let left = if flipped { -width } else { 0.0 };
            ((left, -height - font_h * 0.25), angle.rem_euclid(180.0))
        }
        _ => ((left, top), angle),
    };
    transform.rotation_deg += rotation;
    let mut bounds = SymbolBounds {
        min_x: f64::INFINITY,
        min_y: f64::INFINITY,
        max_x: f64::NEG_INFINITY,
        max_y: f64::NEG_INFINITY,
    };
    for (x, y) in [
        (left, top),
        (left + width, top),
        (left, top + height),
        (left + width, top + height),
    ] {
        let (x, y) = transform_pin(x, -y, transform);
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        bounds.min_x = bounds.min_x.min(x);
        bounds.max_x = bounds.max_x.max(x);
        bounds.min_y = bounds.min_y.min(y);
        bounds.max_y = bounds.max_y.max(y);
    }
    Some(bounds)
}

fn sheet_text<'a>(tree: &'a SexpNode, symbols: &[PlacedSymbol<'a>]) -> Vec<TextItem<'a>> {
    let mut text = Vec::new();
    for kind in ["label", "global_label", "hierarchical_label", "text"] {
        text.extend(
            tree.find_all(kind)
                .into_iter()
                .filter_map(|node| text_item(node, kind, None, None)),
        );
    }
    for symbol in symbols {
        text.extend(
            symbol
                .source
                .find_all("property")
                .into_iter()
                .filter_map(|node| {
                    text_item(node, "symbol_field", Some(symbol.source), Some(symbol))
                }),
        );
    }
    for sheet in tree.find_all("sheet") {
        text.extend(
            sheet
                .find_all("property")
                .into_iter()
                .filter_map(|node| text_item(node, "sheet_field", Some(sheet), None)),
        );
    }
    text
}

fn text_overlap(a: &TextItem<'_>, b: &TextItem<'_>, tolerance: f64) -> Option<Value> {
    let labels = a.is_label() && b.is_label();
    let collides = match (a.bounds, b.bounds) {
        (Some(a), Some(b)) => boxes_overlap(a, b),
        _ => {
            labels
                && a.text != b.text
                && points_coincident(a.anchor.0, a.anchor.1, b.anchor.0, b.anchor.1, tolerance)
        }
    };
    if !collides {
        return None;
    }
    if labels {
        return Some(json!({
            "type": "label_overlap", "net_a": a.text, "net_b": b.text,
            "kind_a": a.kind, "kind_b": b.kind, "uuid_a": a.uuid, "uuid_b": b.uuid,
            "x": round6(a.anchor.0), "y": round6(a.anchor.1),
        }));
    }
    Some(json!({
        "type": "text_overlap", "kind_a": a.kind, "kind_b": b.kind,
        "text_a": a.text, "text_b": b.text, "uuid_a": a.uuid, "uuid_b": b.uuid,
    }))
}

pub(super) fn find_overlaps(tree: &SexpNode, tolerance: f64) -> Value {
    let symbols = placed_symbols(tree);
    let mut findings = Vec::new();
    for (index, a) in symbols.iter().enumerate() {
        findings.extend(
            symbols[index + 1..]
                .iter()
                .filter_map(|b| component_overlap(a, b, tolerance)),
        );
    }

    let text = sheet_text(tree, &symbols);
    let wires = extract_wires(tree);
    for (index, a) in text.iter().enumerate() {
        findings.extend(
            text[index + 1..]
                .iter()
                .filter_map(|b| text_overlap(a, b, tolerance)),
        );
        let Some(bounds) = a.bounds else { continue };
        for symbol in &symbols {
            if a.symbol
                .is_some_and(|owner| std::ptr::eq(owner, symbol.source))
            {
                continue;
            }
            if symbol.body.is_some_and(|body| boxes_overlap(bounds, body)) {
                findings.push(json!({
                    "type": "text_body_overlap", "kind_a": a.kind, "kind_b": "symbol",
                    "text_a": a.text, "reference_b": symbol.instance.reference,
                    "uuid_a": a.uuid, "uuid_b": symbol.instance.uuid,
                }));
            }
        }
        for wire in &wires {
            let segment = Segment {
                start: (wire.x1, wire.y1),
                end: (wire.x2, wire.y2),
            };
            if segment.crosses_interior(bounds) {
                findings.push(json!({
                    "type": "text_wire_overlap", "kind_a": a.kind, "kind_b": "wire",
                    "text_a": a.text, "uuid_a": a.uuid, "uuid_b": wire.uuid,
                }));
            }
        }
    }
    let unresolved: Vec<_> = symbols
        .iter()
        .filter(|symbol| !symbol.bounds_resolved())
        .map(|symbol| symbol.instance.reference.as_str())
        .collect();
    json!({
        "overlap_count": findings.len(), "overlaps": findings,
        "bounds_resolved": symbols.len() - unresolved.len(),
        "bounds_unresolved": unresolved,
    })
}

#[cfg(test)]
mod tests {
    use super::Segment;

    #[test]
    fn touching_pin_tips_are_distinct_from_penetration() {
        let horizontal = Segment {
            start: (0.0, 0.0),
            end: (2.0, 0.0),
        };
        let tip_to_tip = Segment {
            start: (0.0, 0.0),
            end: (0.0, 2.0),
        };
        let tip_on_pin = Segment {
            start: (1.0, 0.0),
            end: (1.0, 2.0),
        };
        let crosses = Segment {
            start: (1.0, -1.0),
            end: (1.0, 1.0),
        };
        let shares_length = Segment {
            start: (1.0, 0.0),
            end: (3.0, 0.0),
        };
        let parallel = Segment {
            start: (1.0, 1.0),
            end: (3.0, 1.0),
        };
        assert!(!horizontal.overlaps_pin(tip_to_tip));
        assert!(!horizontal.overlaps_pin(tip_on_pin));
        assert!(horizontal.overlaps_pin(crosses));
        assert!(horizontal.overlaps_pin(shares_length));
        assert!(!horizontal.overlaps_pin(parallel));
    }
}
