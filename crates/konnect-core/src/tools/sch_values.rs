//! Values a schematic write tool accepts, checked once at the boundary (#132).

use crate::mcp::protocol::CallToolResult;
use serde_json::Value;

use super::{invalid_arg, require_f64, require_str};

/// KiCad stores schematic coordinates as 100 nm integers and clamps every
/// point to `INT_MAX * 0.7071` units, so anything further out loads somewhere
/// other than where it was written.
pub(crate) const SCHEMATIC_COORD_LIMIT_MM: f64 = 151_848.568_7;

/// A coordinate KiCad will load at the written position.
pub(crate) fn check_schematic_coord(value: f64, key: &str) -> Result<f64, CallToolResult> {
    if value.is_finite() && value.abs() <= SCHEMATIC_COORD_LIMIT_MM {
        return Ok(value);
    }
    Err(invalid_arg(
        key,
        &format!(
            "{value} mm is outside the schematic coordinate range \
             -{SCHEMATIC_COORD_LIMIT_MM}..{SCHEMATIC_COORD_LIMIT_MM} mm; KiCad would clamp it"
        ),
    ))
}

pub(crate) fn schematic_coord(args: &Value, key: &str) -> Result<f64, CallToolResult> {
    check_schematic_coord(require_f64(args, key)?, key)
}

/// KiCad clamps pins, not only the symbol origin, so a part placed just inside
/// the limit can still have a pin pulled back. Check every pin `reference`
/// would have in `candidate`.
pub(crate) fn check_pins_within_limit(
    candidate: &str,
    reference: &str,
) -> Result<(), CallToolResult> {
    let Ok(tree) = konnect_sexp::parse_sexp(candidate) else {
        return Ok(());
    };
    let outside = super::placed_pins_by_reference(&tree)
        .into_iter()
        .filter(|(instance, _)| instance.reference == reference)
        .flat_map(|(_, pins)| pins)
        .map(|(pin, transform)| konnect_sexp::schematic::pin_endpoint(&pin, transform))
        .find(|(x, y)| x.abs() > SCHEMATIC_COORD_LIMIT_MM || y.abs() > SCHEMATIC_COORD_LIMIT_MM);
    match outside {
        None => Ok(()),
        Some((x, y)) => Err(invalid_arg(
            if x.abs() > SCHEMATIC_COORD_LIMIT_MM {
                "x"
            } else {
                "y"
            },
            &format!(
                "a pin of {reference} would land at ({x}, {y}), outside the schematic \
                 coordinate range of +-{SCHEMATIC_COORD_LIMIT_MM} mm; KiCad would clamp it"
            ),
        )),
    }
}

/// The page a schematic is drawn on, in millimetres, as `(width, height)`.
/// `None` when the sheet names no size this crate knows.
pub(crate) fn paper_size_mm(source: &str) -> Option<(f64, f64)> {
    let tree = konnect_sexp::parse_sexp(source).ok()?;
    let paper = tree.find("paper")?;
    let name = paper.get(1)?.as_str()?;
    if name == "User" {
        return Some((paper.get_f64(2)?, paper.get_f64(3)?));
    }
    let (width, height) = super::sch_hierarchy::paper_dimensions(name).ok()?;
    let portrait = paper
        .children()?
        .iter()
        .any(|node| node.as_str() == Some("portrait"));
    Some(if portrait {
        (height, width)
    } else {
        (width, height)
    })
}

/// A net name KiCad can show and match: not blank, no control characters.
pub(crate) fn net_name<'a>(args: &'a Value, key: &str) -> Result<&'a str, CallToolResult> {
    let name = require_str(args, key)?;
    if name.trim().is_empty() {
        return Err(invalid_arg(key, "net name must not be empty"));
    }
    if name.chars().any(char::is_control) {
        return Err(invalid_arg(
            key,
            "net name must not contain control characters such as newlines or tabs",
        ));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn coordinate_limit_is_kicads_clamp() {
        assert!(check_schematic_coord(SCHEMATIC_COORD_LIMIT_MM, "x").is_ok());
        assert!(check_schematic_coord(-SCHEMATIC_COORD_LIMIT_MM, "x").is_ok());
        for bad in [300_000.0, -250_000.0, f64::NAN, f64::INFINITY, 1e9] {
            assert!(check_schematic_coord(bad, "x").is_err(), "{bad}");
        }
    }

    #[test]
    fn net_name_refuses_blank_and_control_characters() {
        for bad in ["", "   ", "A\nB", "A\tB"] {
            assert!(net_name(&json!({ "net": bad }), "net").is_err(), "{bad:?}");
        }
        assert_eq!(net_name(&json!({ "net": "VCC" }), "net").unwrap(), "VCC");
    }
}
