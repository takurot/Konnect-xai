//! What a library footprint carries besides its pads, drawings and fields,
//! read into the typed messages KiCad's API takes.
//!
//! Shared by `update_pcb_from_schematic`, which places new footprints, and
//! `update_footprints_from_library`, which refreshes placed ones, so both put
//! the same library data on the board. Before #789 only the refresh read it,
//! and every footprint the sync placed reached the board with no mounting
//! style, no exclusion flags, no description or keywords and no 3D model.

use anyhow::{bail, Context, Result};
use konnect_ipc::gen::kiapi;
use konnect_sexp::SexpNode;

/// The instance attributes a library footprint's `(attr …)` declares.
///
/// A footprint with no `(attr …)` gets KiCad's default: no mounting style,
/// which KiCad treats as unspecified. 17 of the 15,451 footprints KiCad 10.0.5
/// ships have none. An unknown token is refused rather than dropped, because
/// it would change what the board says about the part.
pub(crate) fn attributes(root: &SexpNode) -> Result<kiapi::board::types::FootprintAttributes> {
    use kiapi::board::types::FootprintMountingStyle;

    let mut attributes = kiapi::board::types::FootprintAttributes::default();
    let Some(attr) = root.find("attr") else {
        return Ok(attributes);
    };
    for value in attr.children().unwrap_or_default().iter().skip(1) {
        match value
            .as_str()
            .context("footprint attr contains a non-atom")?
        {
            "smd" => attributes.mounting_style = FootprintMountingStyle::FmsSmd as i32,
            "through_hole" => {
                attributes.mounting_style = FootprintMountingStyle::FmsThroughHole as i32
            }
            "board_only" => attributes.not_in_schematic = true,
            "exclude_from_pos_files" => attributes.exclude_from_position_files = true,
            "exclude_from_bom" => attributes.exclude_from_bill_of_materials = true,
            "allow_missing_courtyard" => attributes.exempt_from_courtyard_requirement = true,
            "dnp" => attributes.do_not_populate = true,
            "allow_soldermask_bridges" => attributes.allow_soldermask_bridges = true,
            unsupported => bail!("footprint attribute '{unsupported}' is not supported"),
        }
    }
    Ok(attributes)
}

/// The library description and keywords. KiCad keeps these on the footprint
/// definition, not the instance, in the same message type as the attributes.
pub(crate) fn description_and_keywords(
    root: &SexpNode,
) -> kiapi::board::types::FootprintAttributes {
    kiapi::board::types::FootprintAttributes {
        description: root.find_str("descr").unwrap_or_default().to_string(),
        keywords: root.find_str("tags").unwrap_or_default().to_string(),
        ..Default::default()
    }
}

/// Every `(model …)`, with its offset, scale, rotation, opacity and
/// visibility.
///
/// A hidden model is written two ways: the bare `hide` atom, and the
/// `(hide yes)` list KiCad 10 writes. 16 models in KiCad 10.0.5's own library
/// use the list form. Both are read; any other clause is refused.
///
/// The vector values go into the `*_nm` fields unconverted, because that is
/// how KiCad's own serializer packs a 3D model's millimetre offset, scale and
/// degrees. An absent opacity is KiCad's default, fully opaque; a present one
/// that is not a number is refused rather than read as that default.
pub(crate) fn models(root: &SexpNode) -> Result<Vec<kiapi::board::types::Footprint3DModel>> {
    root.find_all("model")
        .into_iter()
        .map(|model| {
            let mut hidden = false;
            for child in model.children().unwrap_or_default().iter().skip(2) {
                let Some(tag) = child.head() else {
                    if child.as_str() == Some("hide") {
                        hidden = true;
                        continue;
                    }
                    bail!("3D model contains an unsupported atom");
                };
                match tag {
                    "offset" | "scale" | "rotate" | "opacity" => {}
                    "hide" => {
                        hidden = match child.get(1).and_then(SexpNode::as_str) {
                            Some("yes") => true,
                            Some("no") => false,
                            _ => bail!("3D model hide must be yes or no"),
                        }
                    }
                    _ => bail!("3D model clause '{tag}' is not supported"),
                }
            }
            let vector = |tag: &str, default: [f64; 3]| -> Result<kiapi::common::types::Vector3D> {
                let Some(wrapper) = model.find(tag) else {
                    return Ok(kiapi::common::types::Vector3D {
                        x_nm: default[0],
                        y_nm: default[1],
                        z_nm: default[2],
                    });
                };
                let xyz = wrapper
                    .find("xyz")
                    .with_context(|| format!("3D model {tag} is missing xyz"))?;
                Ok(kiapi::common::types::Vector3D {
                    x_nm: xyz
                        .get_f64(1)
                        .with_context(|| format!("3D model {tag}.x is invalid"))?,
                    y_nm: xyz
                        .get_f64(2)
                        .with_context(|| format!("3D model {tag}.y is invalid"))?,
                    z_nm: xyz
                        .get_f64(3)
                        .with_context(|| format!("3D model {tag}.z is invalid"))?,
                })
            };
            Ok(kiapi::board::types::Footprint3DModel {
                filename: model
                    .get(1)
                    .and_then(SexpNode::as_str)
                    .context("3D model is missing its filename")?
                    .to_string(),
                scale: Some(vector("scale", [1.0, 1.0, 1.0])?),
                rotation: Some(vector("rotate", [0.0, 0.0, 0.0])?),
                offset: Some(vector("offset", [0.0, 0.0, 0.0])?),
                visible: !hidden,
                opacity: match model.find("opacity") {
                    None => 1.0,
                    Some(opacity) => opacity
                        .get_f64(1)
                        .context("3D model opacity is not a number")?,
                },
            })
        })
        .collect()
}

pub(crate) fn validate_supported_children(root: &konnect_sexp::SexpNode) -> Result<()> {
    for child in root.children().unwrap_or_default().iter().skip(2) {
        let Some(tag) = child.head() else {
            continue;
        };
        match tag {
            "version" | "generator" | "generator_version" | "layer" | "descr" | "tags" | "attr"
            | "property" | "fp_text" | "fp_line" | "fp_rect" | "fp_circle" | "fp_arc"
            | "fp_poly" | "pad" | "model" => {}
            // KiCad 10 writes these two flags into every footprint it saves
            // (all 15,428 official library files carry them, every one at its
            // default). At the default the typed rebuild loses nothing;
            // a non-default value carries semantics it cannot represent.
            "embedded_fonts" | "duplicate_pad_numbers_are_jumpers" => {
                let value = child.get(1).and_then(konnect_sexp::SexpNode::as_str);
                if value != Some("no") {
                    bail!(
                        "footprint '{tag} {}' is not supported by typed IPC footprint rebuild",
                        value.unwrap_or("")
                    );
                }
            }
            unsupported => {
                bail!("footprint child '{unsupported}' is not supported by typed IPC footprint rebuild")
            }
        }
        match tag {
            "pad" => validate_pad(child)?,
            "fp_line" | "fp_rect" | "fp_circle" | "fp_arc" | "fp_poly" => {
                validate_graphic(child, tag)?
            }
            "fp_text" => {
                let kind = child
                    .get(1)
                    .and_then(konnect_sexp::SexpNode::as_str)
                    .context("fp_text is missing its kind")?;
                match kind {
                    "reference" | "value" => {}
                    "user" => validate_user_text(child)?,
                    _ => {
                        bail!(
                            "fp_text kind '{kind}' is not supported losslessly by typed IPC footprint rebuild"
                        );
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Accept only the `fp_text user` subset the typed IPC rebuild can reproduce.
/// Refusing an unknown clause is intentional: silently dropping even a visual
/// modifier would make a refreshed board differ from its library definition.
fn validate_user_text(text: &konnect_sexp::SexpNode) -> Result<()> {
    text.get(2)
        .and_then(konnect_sexp::SexpNode::as_str)
        .context("fp_text user is missing its text")?;

    let mut saw_at = false;
    let mut saw_layer = false;
    let mut saw_effects = false;
    let mut identifier = None;
    for clause in text.children().unwrap_or_default().iter().skip(3) {
        let tag = clause
            .head()
            .context("fp_text user contains an unsupported atom")?;
        match tag {
            "at" => {
                if saw_at {
                    bail!("fp_text user contains duplicate 'at' clauses");
                }
                saw_at = true;
                let count = clause.children().map_or(0, |children| children.len());
                if !matches!(count, 3 | 4) {
                    bail!("fp_text user 'at' must contain x, y, and optional rotation");
                }
                for index in 1..count {
                    let value = clause
                        .get_f64(index)
                        .with_context(|| format!("fp_text user 'at' value {index} is invalid"))?;
                    if !value.is_finite() {
                        bail!("fp_text user 'at' values must be finite");
                    }
                }
            }
            "layer" => {
                if saw_layer {
                    bail!("fp_text user contains duplicate 'layer' clauses");
                }
                saw_layer = true;
                if clause.children().map_or(0, |children| children.len()) != 2
                    || clause
                        .get(1)
                        .and_then(konnect_sexp::SexpNode::as_str)
                        .is_none_or(str::is_empty)
                {
                    bail!("fp_text user 'layer' must name exactly one layer");
                }
            }
            "uuid" | "tstamp" => {
                if let Some(previous) = identifier {
                    bail!(
                        "fp_text user contains multiple identifier clauses ('{previous}' and '{tag}')"
                    );
                }
                identifier = Some(tag);
                if clause.children().map_or(0, |children| children.len()) != 2
                    || clause
                        .get(1)
                        .and_then(konnect_sexp::SexpNode::as_str)
                        .is_none_or(str::is_empty)
                {
                    bail!("fp_text user '{tag}' must contain exactly one identifier");
                }
            }
            "effects" => {
                if saw_effects {
                    bail!("fp_text user contains duplicate 'effects' clauses");
                }
                saw_effects = true;
                validate_user_text_effects(clause)?;
            }
            unsupported => {
                bail!("fp_text user clause '{unsupported}' is not supported losslessly");
            }
        }
    }

    if !saw_at {
        bail!("fp_text user is missing its 'at' clause");
    }
    if !saw_layer {
        bail!("fp_text user is missing its 'layer' clause");
    }
    if !saw_effects {
        bail!("fp_text user is missing its 'effects' clause");
    }
    Ok(())
}

fn validate_user_text_effects(effects: &konnect_sexp::SexpNode) -> Result<()> {
    let mut font = None;
    for clause in effects.children().unwrap_or_default().iter().skip(1) {
        let tag = clause
            .head()
            .context("fp_text user effects contain an unsupported atom")?;
        if tag != "font" {
            bail!("fp_text user effects clause '{tag}' is not supported losslessly");
        }
        if font.replace(clause).is_some() {
            bail!("fp_text user contains duplicate font clauses");
        }
    }
    let font = font.context("fp_text user effects are missing the font clause")?;

    let mut size = None;
    let mut thickness = None;
    for clause in font.children().unwrap_or_default().iter().skip(1) {
        let tag = clause
            .head()
            .context("fp_text user font contains an unsupported atom")?;
        match tag {
            "size" => {
                if size.is_some() {
                    bail!("fp_text user contains duplicate font size clauses");
                }
                if clause.children().map_or(0, |children| children.len()) != 3 {
                    bail!("fp_text user font size must contain width and height");
                }
                let width = clause
                    .get_f64(1)
                    .context("fp_text user font width is invalid")?;
                let height = clause
                    .get_f64(2)
                    .context("fp_text user font height is invalid")?;
                if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
                    bail!("fp_text user font size must be finite and positive");
                }
                if width != height {
                    bail!("fp_text user non-square font size is not supported losslessly");
                }
                size = Some(width);
            }
            "thickness" => {
                if thickness.is_some() {
                    bail!("fp_text user contains duplicate font thickness clauses");
                }
                if clause.children().map_or(0, |children| children.len()) != 2 {
                    bail!("fp_text user font thickness must contain exactly one value");
                }
                let value = clause
                    .get_f64(1)
                    .context("fp_text user font thickness is invalid")?;
                if !value.is_finite() || value <= 0.0 {
                    bail!("fp_text user font thickness must be finite and positive");
                }
                thickness = Some(value);
            }
            unsupported => {
                bail!("fp_text user font clause '{unsupported}' is not supported losslessly");
            }
        }
    }
    size.context("fp_text user font is missing its size")?;
    thickness.context("fp_text user font is missing its thickness")?;
    Ok(())
}

fn validate_graphic(graphic: &konnect_sexp::SexpNode, kind: &str) -> Result<()> {
    let allowed: &[&str] = match kind {
        "fp_line" => &["start", "end", "stroke", "layer", "uuid", "tstamp"],
        "fp_rect" => &["start", "end", "stroke", "fill", "layer", "uuid", "tstamp"],
        "fp_circle" => &["center", "end", "stroke", "fill", "layer", "uuid", "tstamp"],
        "fp_arc" => &["start", "mid", "end", "stroke", "layer", "uuid", "tstamp"],
        "fp_poly" => &["pts", "stroke", "fill", "layer", "uuid", "tstamp"],
        _ => bail!("unsupported footprint graphic '{kind}'"),
    };
    for child in graphic.children().unwrap_or_default().iter().skip(1) {
        let Some(tag) = child.head() else {
            bail!("{kind} contains an unsupported atom");
        };
        if !allowed.contains(&tag) {
            bail!("{kind} clause '{tag}' is not supported by typed IPC footprint rebuild");
        }
        match tag {
            "stroke" => {
                for stroke_child in child.children().unwrap_or_default().iter().skip(1) {
                    let stroke_tag = stroke_child
                        .head()
                        .context("graphic stroke contains an unsupported atom")?;
                    match stroke_tag {
                        "width" => {}
                        "type" => {
                            let stroke_type = stroke_child
                                .get(1)
                                .and_then(konnect_sexp::SexpNode::as_str)
                                .context("graphic stroke type is missing")?;
                            if !matches!(
                                stroke_type,
                                "solid" | "default" | "dash" | "dot" | "dash_dot" | "dash_dot_dot"
                            ) {
                                bail!(
                                    "graphic stroke type '{stroke_type}' is not supported losslessly"
                                );
                            }
                        }
                        unsupported => bail!(
                            "graphic stroke clause '{unsupported}' is not supported losslessly"
                        ),
                    }
                }
            }
            "fill" => {
                let fill = child
                    .get(1)
                    .and_then(konnect_sexp::SexpNode::as_str)
                    .context("graphic fill is missing")?;
                if !matches!(fill, "none" | "no" | "solid" | "yes") {
                    bail!("graphic fill '{fill}' is not supported losslessly");
                }
            }
            "pts"
                if child
                    .children()
                    .unwrap_or_default()
                    .iter()
                    .skip(1)
                    .any(|point| point.head() != Some("xy")) =>
            {
                bail!("fp_poly contains a non-xy point");
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_pad(pad: &konnect_sexp::SexpNode) -> Result<()> {
    let shape = pad
        .get(3)
        .and_then(konnect_sexp::SexpNode::as_str)
        .context("footprint pad is missing its shape")?;
    if !matches!(
        shape,
        "circle" | "rect" | "oval" | "roundrect" | "chamfered_rect"
    ) {
        bail!("pad shape '{shape}' is not supported by typed IPC footprint rebuild");
    }
    for child in pad.children().unwrap_or_default().iter().skip(4) {
        let Some(tag) = child.head() else {
            continue;
        };
        if !matches!(
            tag,
            "at" | "size"
                | "layers"
                | "drill"
                | "roundrect_rratio"
                | "chamfer_ratio"
                | "chamfer"
                | "uuid"
                | "tstamp"
                | "remove_unused_layers"
        ) {
            bail!("pad clause '{tag}' is not supported by typed IPC footprint rebuild");
        }
        if tag == "remove_unused_layers" {
            // The typed rebuild always keeps unused layers (`UlrKeep`), so a
            // pad that actually removes them cannot be rebuilt losslessly.
            let value = child.get(1).and_then(konnect_sexp::SexpNode::as_str);
            if value != Some("no") {
                bail!("pad 'remove_unused_layers yes' is not supported by typed IPC footprint rebuild");
            }
        }
        if tag == "drill" {
            for nested in child.children().unwrap_or_default().iter().skip(1) {
                if nested.head().is_some() {
                    bail!("nested drill clauses are not supported by typed IPC footprint rebuild");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiapi::board::types::FootprintMountingStyle;

    const C_0603: &str = include_str!("../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");

    fn root(source: &str) -> SexpNode {
        konnect_sexp::parse_sexp(source).unwrap()
    }

    /// The KiCad-written 0603 capacitor: `(attr smd)`, its description and
    /// tags, and one visible model with identity transforms.
    #[test]
    fn a_kicad_library_footprint_reads_whole() {
        let root = root(C_0603);

        let attributes = attributes(&root).unwrap();
        assert_eq!(
            attributes.mounting_style,
            FootprintMountingStyle::FmsSmd as i32
        );
        assert!(!attributes.exclude_from_position_files);
        assert!(!attributes.do_not_populate);

        let text = description_and_keywords(&root);
        assert!(text
            .description
            .starts_with("Capacitor SMD 0603 (1608 Metric)"));
        assert_eq!(text.keywords, "capacitor");

        let models = models(&root).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0].filename,
            "${KICAD10_3DMODEL_DIR}/Capacitor_SMD.3dshapes/C_0603_1608Metric.step"
        );
        assert!(models[0].visible);
        assert_eq!(models[0].opacity, 1.0);
        let scale = models[0].scale.as_ref().unwrap();
        assert_eq!((scale.x_nm, scale.y_nm, scale.z_nm), (1.0, 1.0, 1.0));
    }

    /// Where KiCad's own footprint libraries are installed, if they are.
    fn installed_footprint_dir() -> Option<std::path::PathBuf> {
        ["KICAD10_FOOTPRINT_DIR", "KICAD_FOOTPRINT_DIR"]
            .iter()
            .filter_map(std::env::var_os)
            .map(std::path::PathBuf::from)
            .chain(
                [
                    r"C:\KiCad\10.0\share\kicad\footprints",
                    r"C:\Program Files\KiCad\10.0\share\kicad\footprints",
                    "/usr/share/kicad/footprints",
                    "/usr/local/share/kicad/footprints",
                    "/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints",
                ]
                .iter()
                .map(std::path::PathBuf::from),
            )
            .find(|dir| dir.is_dir())
    }

    /// KiCad 10 hides a model with `(hide yes)`. Read from the installed
    /// library rather than committed, because the library is CC BY-SA (the
    /// same choice as #738's demo sheet). Skips without an installed KiCad.
    #[test]
    fn a_model_hidden_the_kicad_10_way_reads_as_hidden() {
        let Some(dir) = installed_footprint_dir() else {
            eprintln!("SKIP: no installed KiCad footprint library");
            return;
        };
        let path = dir.join("Package_SO.pretty/Texas_S-PDSO-G8_3x3mm_P0.65mm.kicad_mod");
        let Ok(source) = std::fs::read_to_string(&path) else {
            eprintln!("SKIP: {} is not installed", path.display());
            return;
        };
        let models = models(&root(&source)).unwrap();
        assert_eq!(models.len(), 1);
        assert!(
            !models[0].visible,
            "{} is hidden in the library",
            models[0].filename
        );
    }

    /// Opacity defaults only when the library leaves it out. A value that is
    /// there and readable is kept; one that is there and unreadable refuses the
    /// model, where it used to become full opacity without a word.
    #[test]
    fn opacity_defaults_only_when_absent() {
        let with = |opacity: &str| {
            root(&format!(
                "(footprint \"X\" (model \"m.step\" (offset (xyz 0 0 0)) {opacity}))"
            ))
        };
        assert_eq!(models(&with("")).unwrap()[0].opacity, 1.0);
        assert_eq!(models(&with("(opacity 0.4)")).unwrap()[0].opacity, 0.4);
        let refusal = models(&with("(opacity dim)")).unwrap_err();
        assert!(
            format!("{refusal:#}").contains("3D model opacity is not a number"),
            "{refusal:#}"
        );
        assert!(models(&with("(opacity)")).is_err());
    }

    #[test]
    fn hide_no_is_visible_and_anything_else_is_refused() {
        let with = |hide: &str| {
            root(&format!(
                "(footprint \"X\" (model \"m.step\" (offset (xyz 0 0 0)) {hide}))"
            ))
        };
        assert!(models(&with("(hide no)")).unwrap()[0].visible);
        assert!(!models(&with("(hide yes)")).unwrap()[0].visible);
        assert!(!models(&with("hide")).unwrap()[0].visible);
        assert!(models(&with("(hide maybe)")).is_err());
    }
}

fn parse_library_property(
    property: &konnect_sexp::SexpNode,
    require_typed_field: bool,
) -> Result<Option<kiapi::board::types::Field>> {
    use kiapi::common::types::LockedState;

    let name = property
        .get(1)
        .and_then(konnect_sexp::SexpNode::as_str)
        .context("property is missing its name")?;
    let value = property
        .get(2)
        .and_then(konnect_sexp::SexpNode::as_str)
        .with_context(|| format!("property '{name}' is missing its value"))?;
    let mut position = None;
    let mut rotation = 0.0;
    let mut layer = None;
    let mut hidden = None;
    let mut knockout = None;
    let mut attributes = None;
    let mut identifier = None;

    for clause in property.children().unwrap_or_default().iter().skip(3) {
        let tag = clause
            .head()
            .with_context(|| format!("property '{name}' contains an unsupported atom"))?;
        match tag {
            "at" => {
                if position.is_some() {
                    bail!("property '{name}' contains duplicate 'at' clauses");
                }
                let count = clause.children().map_or(0, |children| children.len());
                if !matches!(count, 3 | 4) {
                    bail!("property '{name}' 'at' must contain x, y, and optional rotation");
                }
                let x = clause
                    .get_f64(1)
                    .with_context(|| format!("property '{name}' has an invalid X position"))?;
                let y = clause
                    .get_f64(2)
                    .with_context(|| format!("property '{name}' has an invalid Y position"))?;
                rotation = clause.get_f64(3).unwrap_or(0.0);
                if !x.is_finite() || !y.is_finite() || !rotation.is_finite() {
                    bail!("property '{name}' position and rotation must be finite");
                }
                position = Some(konnect_ipc::builders::vec2(x, y));
            }
            "layer" => {
                if layer.is_some() {
                    bail!("property '{name}' contains duplicate 'layer' clauses");
                }
                let layer_name = clause
                    .get(1)
                    .and_then(konnect_sexp::SexpNode::as_str)
                    .filter(|name| !name.is_empty())
                    .with_context(|| format!("property '{name}' has no layer name"))?;
                if clause.children().map_or(0, |children| children.len()) != 2 {
                    bail!("property '{name}' 'layer' must name exactly one layer");
                }
                layer = Some(
                    konnect_ipc::builders::try_layer_from_name(layer_name)
                        .with_context(|| format!("property '{name}' has an unsupported layer"))?
                        as i32,
                );
            }
            "hide" => {
                if hidden.is_some() {
                    bail!("property '{name}' contains duplicate 'hide' clauses");
                }
                hidden = Some(property_yes_no(clause, name, "hide")?);
            }
            "knockout" => {
                if knockout.is_some() {
                    bail!("property '{name}' contains duplicate 'knockout' clauses");
                }
                knockout = Some(property_yes_no(clause, name, "knockout")?);
            }
            "uuid" | "tstamp" => {
                if let Some(previous) = identifier {
                    bail!(
                        "property '{name}' contains multiple identifier clauses ('{previous}' and '{tag}')"
                    );
                }
                identifier = Some(tag);
                if clause.children().map_or(0, |children| children.len()) != 2
                    || clause
                        .get(1)
                        .and_then(konnect_sexp::SexpNode::as_str)
                        .is_none_or(str::is_empty)
                {
                    bail!("property '{name}' '{tag}' must contain exactly one identifier");
                }
            }
            "effects" => {
                if attributes.is_some() {
                    bail!("property '{name}' contains duplicate 'effects' clauses");
                }
                attributes = Some(parse_property_effects(clause, name)?);
            }
            unsupported => {
                bail!("property '{name}' clause '{unsupported}' is not supported losslessly")
            }
        }
    }

    let position = match position {
        Some(position) => position,
        None if !require_typed_field => konnect_ipc::builders::vec2(0.0, 0.0),
        None => bail!("property '{name}' is missing its 'at' clause"),
    };
    let layer = match layer {
        Some(layer) => layer,
        None if !require_typed_field => kiapi::board::types::BoardLayer::BlFFab as i32,
        None => bail!("property '{name}' is missing its 'layer' clause"),
    };
    let mut attributes = match attributes {
        Some(attributes) => attributes,
        None if !require_typed_field => kiapi::common::types::TextAttributes {
            size: Some(konnect_ipc::builders::vec2(1.0, 1.0)),
            stroke_width: Some(konnect_ipc::builders::distance(0.15)),
            line_spacing: 1.0,
            keep_upright: true,
            multiline: true,
            visible: true,
            horizontal_alignment: kiapi::common::types::HorizontalAlignment::HaCenter as i32,
            vertical_alignment: kiapi::common::types::VerticalAlignment::VaCenter as i32,
            ..Default::default()
        },
        None => bail!("property '{name}' is missing its 'effects' clause"),
    };
    attributes.angle = Some(kiapi::common::types::Angle {
        value_degrees: rotation,
    });
    Ok(Some(kiapi::board::types::Field {
        id: None,
        name: name.to_string(),
        text: Some(kiapi::board::types::BoardText {
            // A library child's UUID is definition-local and cannot be reused
            // across placed instances. Let KiCad assign the board child ID.
            id: None,
            text: Some(kiapi::common::types::Text {
                position: Some(position),
                attributes: Some(attributes),
                text: value.to_string(),
                hyperlink: String::new(),
            }),
            layer,
            knockout: knockout.unwrap_or(false),
            locked: LockedState::LsUnlocked as i32,
            parent: None,
        }),
        visible: !hidden.unwrap_or(false),
    }))
}

fn property_yes_no(clause: &konnect_sexp::SexpNode, name: &str, tag: &str) -> Result<bool> {
    if clause.children().map_or(0, |children| children.len()) != 2 {
        bail!("property '{name}' '{tag}' must contain exactly one yes/no value");
    }
    match clause.get(1).and_then(konnect_sexp::SexpNode::as_str) {
        Some("yes") => Ok(true),
        Some("no") => Ok(false),
        _ => bail!("property '{name}' '{tag}' must be yes or no"),
    }
}

fn parse_property_effects(
    effects: &konnect_sexp::SexpNode,
    name: &str,
) -> Result<kiapi::common::types::TextAttributes> {
    use kiapi::common::types::{HorizontalAlignment, VerticalAlignment};

    let mut font = None;
    let mut horizontal = HorizontalAlignment::HaCenter;
    let mut vertical = VerticalAlignment::VaCenter;
    let mut mirrored = false;
    for clause in effects.children().unwrap_or_default().iter().skip(1) {
        let tag = clause
            .head()
            .with_context(|| format!("property '{name}' effects contain an unsupported atom"))?;
        match tag {
            "font" => {
                if font.replace(clause).is_some() {
                    bail!("property '{name}' contains duplicate font clauses");
                }
            }
            "justify" => {
                for value in clause.children().unwrap_or_default().iter().skip(1) {
                    match value.as_str().with_context(|| {
                        format!("property '{name}' justify contains a non-atom")
                    })? {
                        "left" if horizontal == HorizontalAlignment::HaCenter => {
                            horizontal = HorizontalAlignment::HaLeft
                        }
                        "right" if horizontal == HorizontalAlignment::HaCenter => {
                            horizontal = HorizontalAlignment::HaRight
                        }
                        "top" if vertical == VerticalAlignment::VaCenter => {
                            vertical = VerticalAlignment::VaTop
                        }
                        "bottom" if vertical == VerticalAlignment::VaCenter => {
                            vertical = VerticalAlignment::VaBottom
                        }
                        "mirror" if !mirrored => mirrored = true,
                        "left" | "right" => bail!(
                            "property '{name}' has conflicting horizontal justification"
                        ),
                        "top" | "bottom" => {
                            bail!("property '{name}' has conflicting vertical justification")
                        }
                        "mirror" => bail!("property '{name}' repeats mirrored justification"),
                        unsupported => bail!(
                            "property '{name}' justification '{unsupported}' is not supported losslessly"
                        ),
                    }
                }
            }
            unsupported => bail!(
                "property '{name}' effects clause '{unsupported}' is not supported losslessly"
            ),
        }
    }
    let font =
        font.with_context(|| format!("property '{name}' effects are missing the font clause"))?;

    let mut font_name = String::new();
    let mut size = None;
    let mut thickness = None;
    let mut bold = None;
    let mut italic = None;
    let mut line_spacing = None;
    for clause in font.children().unwrap_or_default().iter().skip(1) {
        let tag = clause
            .head()
            .with_context(|| format!("property '{name}' font contains an unsupported atom"))?;
        match tag {
            "face" => {
                if !font_name.is_empty() {
                    bail!("property '{name}' contains duplicate font face clauses");
                }
                font_name = clause
                    .get(1)
                    .and_then(konnect_sexp::SexpNode::as_str)
                    .filter(|face| !face.is_empty())
                    .with_context(|| format!("property '{name}' font face is invalid"))?
                    .to_string();
            }
            "size" => {
                if size.is_some() {
                    bail!("property '{name}' contains duplicate font size clauses");
                }
                if clause.children().map_or(0, |children| children.len()) != 3 {
                    bail!("property '{name}' font size must contain width and height");
                }
                let width = clause
                    .get_f64(1)
                    .with_context(|| format!("property '{name}' font width is invalid"))?;
                let height = clause
                    .get_f64(2)
                    .with_context(|| format!("property '{name}' font height is invalid"))?;
                if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
                    bail!("property '{name}' font size must be finite and positive");
                }
                size = Some((width, height));
            }
            "thickness" => {
                if thickness.is_some() {
                    bail!("property '{name}' contains duplicate font thickness clauses");
                }
                let value = clause
                    .get_f64(1)
                    .with_context(|| format!("property '{name}' font thickness is invalid"))?;
                if clause.children().map_or(0, |children| children.len()) != 2
                    || !value.is_finite()
                    || value <= 0.0
                {
                    bail!("property '{name}' font thickness must be finite and positive");
                }
                thickness = Some(value);
            }
            "bold" => {
                if bold.is_some() {
                    bail!("property '{name}' contains duplicate font 'bold' clauses");
                }
                bold = Some(property_yes_no(clause, name, "font bold")?);
            }
            "italic" => {
                if italic.is_some() {
                    bail!("property '{name}' contains duplicate font 'italic' clauses");
                }
                italic = Some(property_yes_no(clause, name, "font italic")?);
            }
            "line_spacing" => {
                if line_spacing.is_some() {
                    bail!("property '{name}' contains duplicate font 'line_spacing' clauses");
                }
                let value = clause
                    .get_f64(1)
                    .with_context(|| format!("property '{name}' line spacing is invalid"))?;
                if clause.children().map_or(0, |children| children.len()) != 2
                    || !value.is_finite()
                    || value <= 0.0
                {
                    bail!("property '{name}' line spacing must be finite and positive");
                }
                line_spacing = Some(value);
            }
            unsupported => {
                bail!("property '{name}' font clause '{unsupported}' is not supported losslessly")
            }
        }
    }
    let (width, height) =
        size.with_context(|| format!("property '{name}' font is missing its size"))?;
    Ok(kiapi::common::types::TextAttributes {
        font_name,
        horizontal_alignment: horizontal as i32,
        vertical_alignment: vertical as i32,
        angle: None,
        line_spacing: line_spacing.unwrap_or(1.0),
        stroke_width: Some(konnect_ipc::builders::distance(
            thickness.unwrap_or(width * 0.15),
        )),
        italic: italic.unwrap_or(false),
        bold: bold.unwrap_or(false),
        underlined: false,
        visible: true,
        mirrored,
        multiline: true,
        keep_upright: true,
        size: Some(konnect_ipc::builders::vec2(width, height)),
    })
}

pub(crate) fn fields(root: &SexpNode) -> Result<Vec<kiapi::board::types::Field>> {
    let mut fields = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for property in root.find_all("property") {
        let name = property
            .get(1)
            .and_then(SexpNode::as_str)
            .context("property is missing its name")?;
        if name == SCHEMATIC_FIELDS {
            bail!("library property '{name}' is reserved for schematic ownership");
        }
        let mandatory = matches!(name, "Reference" | "Value" | "Datasheet" | "Description");
        let field =
            parse_library_property(property, !mandatory)?.context("missing typed library field")?;
        if !names.insert(name.to_string()) {
            bail!("property '{name}' appears more than once in the library footprint");
        }
        fields.push(field);
    }
    for text in root.find_all("fp_text") {
        let name = match text.get(1).and_then(SexpNode::as_str) {
            Some("reference") => "Reference",
            Some("value") => "Value",
            _ => continue,
        };
        if !names.insert(name.to_string()) {
            bail!("property '{name}' appears more than once in the library footprint");
        }
        let mut property = vec![
            SexpNode::Atom("property".into()),
            SexpNode::Str(name.into()),
        ];
        property.extend_from_slice(&text.children().context("invalid fp_text")?[2..]);
        let mut hidden = false;
        let mut index = 0;
        property.retain(|node| {
            let is_clause = index >= 3;
            index += 1;
            if is_clause && node.as_str() == Some("hide") {
                hidden = true;
                false
            } else {
                true
            }
        });
        if hidden {
            property.push(SexpNode::List(vec![
                SexpNode::Atom("hide".into()),
                SexpNode::Atom("yes".into()),
            ]));
        }
        let field = parse_library_property(&SexpNode::List(property), false)?
            .context("missing typed library field")?;
        fields.push(field);
    }
    Ok(fields)
}

pub(crate) const SCHEMATIC_FIELDS: &str = "Konnect:schematic_fields";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SchematicFields {
    version: u8,
    fields: Vec<(String, String)>,
}

pub(crate) fn field_values(
    instance: &kiapi::board::types::FootprintInstance,
) -> Result<std::collections::BTreeMap<String, String>> {
    use prost::Message;
    let mut values = std::collections::BTreeMap::new();
    if let Some(definition) = &instance.definition {
        for item in &definition.items {
            if !konnect_ipc::builders::any_is(item, "kiapi.board.types.Field") {
                continue;
            }
            let field = kiapi::board::types::Field::decode(item.value.as_slice())
                .context("unreadable footprint field")?;
            if field.name.is_empty()
                || values
                    .insert(field.name.clone(), text_value(&field))
                    .is_some()
            {
                bail!("duplicate or unnamed footprint property '{}'", field.name);
            }
        }
        for (name, primary, fallback) in [
            (
                "Datasheet",
                &instance.datasheet_field,
                &definition.datasheet_field,
            ),
            (
                "Description",
                &instance.description_field,
                &definition.description_field,
            ),
        ] {
            if let Some(field) = primary.as_ref().or(fallback.as_ref()) {
                if values.insert(name.into(), text_value(field)).is_some() {
                    bail!("duplicate footprint property '{name}'");
                }
            }
        }
    }
    Ok(values)
}

fn text_value(field: &kiapi::board::types::Field) -> String {
    field
        .text
        .as_ref()
        .and_then(|text| text.text.as_ref())
        .map_or_else(String::new, |text| text.text.clone())
}

pub(crate) fn schematic_properties(
    existing: &std::collections::BTreeMap<String, String>,
    desired: &std::collections::BTreeMap<String, String>,
) -> Result<std::collections::BTreeMap<String, String>> {
    let valid_name = |name: &str| {
        !name.is_empty() && !matches!(name, SCHEMATIC_FIELDS | "Reference" | "Value" | "Footprint")
    };
    for name in desired.keys() {
        if !valid_name(name) {
            bail!("reserved schematic field '{name}'");
        }
    }
    let mut merged = existing.clone();
    if let Some(record) = existing.get(SCHEMATIC_FIELDS) {
        if record.len() > 65_536 {
            bail!("schematic field ownership record is too large");
        }
        let previous: SchematicFields =
            serde_json::from_str(record).context("invalid schematic field ownership record")?;
        if previous.version != 1 {
            bail!(
                "unsupported schematic field ownership version {}",
                previous.version
            );
        }
        let mut names = std::collections::BTreeSet::new();
        for (name, value) in previous.fields {
            if !valid_name(&name) || !names.insert(name.clone()) {
                bail!("invalid or duplicate managed schematic field '{name}'");
            }
            if !desired.contains_key(&name) && existing.get(&name) == Some(&value) {
                if matches!(name.as_str(), "Datasheet" | "Description") {
                    merged.insert(name, String::new());
                } else {
                    merged.remove(&name);
                }
            }
        }
    }
    merged.extend(desired.clone());
    if !desired.is_empty() || existing.contains_key(SCHEMATIC_FIELDS) {
        let record = SchematicFields {
            version: 1,
            fields: desired
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        };
        let record = serde_json::to_string(&record)?;
        if record.len() > 65_536 {
            bail!("schematic field ownership record is too large");
        }
        merged.insert(SCHEMATIC_FIELDS.into(), record);
    }
    Ok(merged)
}

pub(crate) fn apply_properties(
    instance: &mut kiapi::board::types::FootprintInstance,
    values: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    use prost::Message;
    let template = instance.value_field.clone().unwrap_or_default();
    let definition = instance
        .definition
        .as_mut()
        .context("footprint has no definition")?;
    let mut fields = std::collections::BTreeMap::new();
    let mut retained = Vec::new();
    for item in std::mem::take(&mut definition.items) {
        if konnect_ipc::builders::any_is(&item, "kiapi.board.types.Field") {
            let field = kiapi::board::types::Field::decode(item.value.as_slice())?;
            if fields.insert(field.name.clone(), field).is_some() {
                bail!("duplicate footprint custom property");
            }
        } else {
            retained.push(item);
        }
    }
    let make = |name: &str, value: &str, mut field: kiapi::board::types::Field| {
        field.name = name.into();
        field
            .text
            .get_or_insert_with(Default::default)
            .text
            .get_or_insert_with(Default::default)
            .text = value.into();
        field
    };
    for (name, value) in values {
        match name.as_str() {
            "Datasheet" | "Description" => {}
            _ => {
                let field = match fields.remove(name) {
                    Some(field) => field,
                    None => {
                        let mut field = template.clone();
                        field.id = None;
                        field.visible = false;
                        if let Some(text) = field.text.as_mut() {
                            text.id = None;
                        }
                        field
                    }
                };
                let mut field = make(name, value, field);
                if name == SCHEMATIC_FIELDS {
                    field.visible = false;
                }
                retained.push(konnect_ipc::builders::pack_any(
                    &field,
                    "kiapi.board.types.Field",
                ));
            }
        }
    }
    definition.items = retained;
    for (name, instance_field, definition_field) in [
        (
            "Datasheet",
            &mut instance.datasheet_field,
            &mut definition.datasheet_field,
        ),
        (
            "Description",
            &mut instance.description_field,
            &mut definition.description_field,
        ),
    ] {
        if let Some(value) = values.get(name) {
            let field = instance_field
                .as_ref()
                .or(definition_field.as_ref())
                .cloned()
                .unwrap_or_else(|| {
                    let mut field = template.clone();
                    field.visible = false;
                    field.id = None;
                    field
                });
            let field = make(name, value, field);
            *instance_field = Some(field.clone());
            *definition_field = Some(field);
        } else {
            *instance_field = None;
            *definition_field = None;
        }
    }
    Ok(())
}

pub(crate) fn live_loss_diagnostics(
    source: &str,
) -> Result<std::collections::BTreeMap<String, String>> {
    let root = konnect_sexp::parse_sexp(source).context("live PCB snapshot could not be parsed")?;
    if root.head() != Some("kicad_pcb") {
        bail!("live snapshot is not a PCB");
    }
    let mut blocked = std::collections::BTreeMap::new();
    fn unsupported(node: &SexpNode) -> Option<&str> {
        if matches!(
            node.head(),
            Some("group" | "point" | "variant" | "variants")
        ) {
            return node.head();
        }
        node.children()
            .unwrap_or_default()
            .iter()
            .find_map(unsupported)
    }
    for footprint in root.find_all("footprint") {
        let id = footprint
            .find_str("uuid")
            .or_else(|| footprint.find_str("tstamp"))
            .context("live footprint has no UUID")?;
        if let Some(clause) = footprint
            .children()
            .unwrap_or_default()
            .iter()
            .skip(2)
            .find_map(unsupported)
        {
            blocked.insert(
                id.into(),
                format!("footprint {id} contains unsupported '{clause}' state"),
            );
        }
    }
    for group in root.find_all("group") {
        let name = group.get(1).and_then(SexpNode::as_str).unwrap_or("unnamed");
        if let Some(members) = group.find("members") {
            for id in members.children().unwrap_or_default().iter().skip(1) {
                let id = id
                    .as_str()
                    .context("live group contains an invalid member")?;
                blocked.insert(
                    id.into(),
                    format!("footprint {id} belongs to group '{name}'"),
                );
            }
        }
    }
    Ok(blocked)
}

#[cfg(test)]
mod issue26_tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn ownership_removes_only_unchanged_managed_fields() {
        let initial = BTreeMap::from([("BoardOnly".into(), "keep".into())]);
        let desired = BTreeMap::from([
            ("LCSC".into(), "C25804".into()),
            ("MPN".into(), "old".into()),
        ]);
        let first = schematic_properties(&initial, &desired).unwrap();
        assert_eq!(first["BoardOnly"], "keep");
        assert_eq!(first["LCSC"], "C25804");
        let desired = BTreeMap::from([("LCSC".into(), "C25805".into())]);
        let second = schematic_properties(&first, &desired).unwrap();
        assert_eq!(second["LCSC"], "C25805");
        assert!(!second.contains_key("MPN"));
        assert_eq!(second["BoardOnly"], "keep");
        assert_eq!(schematic_properties(&second, &desired).unwrap(), second);
        let mut edited = first;
        edited.insert("MPN".into(), "board edit".into());
        assert_eq!(
            schematic_properties(&edited, &desired).unwrap()["MPN"],
            "board edit"
        );
    }

    #[test]
    fn first_sync_preserves_unknown_fields_and_empty_sync_is_noop() {
        let initial = BTreeMap::from([("BoardOnly".into(), "keep".into())]);
        assert_eq!(
            schematic_properties(&initial, &BTreeMap::new()).unwrap(),
            initial
        );
        assert_eq!(
            schematic_properties(&BTreeMap::new(), &BTreeMap::new()).unwrap(),
            BTreeMap::new()
        );
    }

    #[test]
    fn corrupt_or_colliding_ownership_is_refused() {
        for record in [
            "invalid",
            r#"{"version":2,"fields":[]}"#,
            r#"{"version":1,"fields":[["LCSC","a"],["LCSC","b"]]}"#,
            r#"{"version":1,"fields":[["Reference","R1"]]}"#,
            r#"{"version":1,"fields":[],"extra":true}"#,
        ] {
            let values = BTreeMap::from([(SCHEMATIC_FIELDS.into(), record.into())]);
            assert!(
                schematic_properties(&values, &BTreeMap::new()).is_err(),
                "{record}"
            );
        }
        assert!(schematic_properties(
            &BTreeMap::new(),
            &BTreeMap::from([(SCHEMATIC_FIELDS.into(), "collision".into())])
        )
        .is_err());
    }

    #[test]
    fn native_field_roundtrip_keeps_board_only_values_and_hides_ownership() {
        let root = konnect_sexp::parse_sexp(include_str!(
            "../../tests/fixtures/socket_kicad10.kicad_mod"
        ))
        .unwrap();
        let mut instance = kiapi::board::types::FootprintInstance {
            definition: Some(kiapi::board::types::Footprint::default()),
            ..Default::default()
        };
        let parsed = fields(&root).unwrap();
        instance.value_field = parsed.iter().find(|field| field.name == "Value").cloned();
        let wanted = schematic_properties(
            &BTreeMap::from([("BoardOnly".into(), "keep".into())]),
            &BTreeMap::from([
                ("LCSC".into(), "C25804".into()),
                ("Datasheet".into(), "https://example.test/a?x=\"1\"".into()),
            ]),
        )
        .unwrap();
        apply_properties(&mut instance, &wanted).unwrap();
        assert_eq!(field_values(&instance).unwrap(), wanted);
        use prost::Message;
        let ownership = instance
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|item| konnect_ipc::builders::any_is(item, "kiapi.board.types.Field"))
            .map(|item| kiapi::board::types::Field::decode(item.value.as_slice()).unwrap())
            .find(|field| field.name == SCHEMATIC_FIELDS)
            .unwrap();
        assert!(!ownership.visible);
        let position = ownership.text.unwrap().text.unwrap().position.unwrap();
        assert_eq!(position.y_nm, 4_000_000);
    }

    #[test]
    fn live_loss_findings_cover_unsaved_groups_points_and_variants() {
        let source = r#"(kicad_pcb
            (footprint "Test:One" (uuid "one") (point (at 0 0)))
            (footprint "Test:Two" (uuid "two") (instances (variant (name "Lite"))))
            (footprint "Test:Three" (uuid "three"))
            (footprint "Test:Safe" (uuid "safe"))
            (group "Assembly" (members "three")))"#;
        let blocked = live_loss_diagnostics(source).unwrap();
        assert!(blocked["one"].contains("point"));
        assert!(blocked["two"].contains("variant"));
        assert!(blocked["three"].contains("Assembly"));
        assert!(!blocked.contains_key("safe"));
        assert!(live_loss_diagnostics("(not_a_board)").is_err());
        assert!(live_loss_diagnostics("(kicad_pcb (footprint \"MissingUuid\"))").is_err());
    }

    #[test]
    fn library_fields_preserve_authored_effects_layers_and_defaults() {
        let root = konnect_sexp::parse_sexp(include_str!(
            "../../tests/fixtures/socket_kicad10.kicad_mod"
        ))
        .unwrap();
        let fields = fields(&root).unwrap();
        let value = fields.iter().find(|field| field.name == "Value").unwrap();
        let text = value.text.as_ref().unwrap();
        assert_eq!(text.layer, kiapi::board::types::BoardLayer::BlFFab as i32);
        let attributes = text.text.as_ref().unwrap().attributes.as_ref().unwrap();
        assert_eq!(attributes.stroke_width.as_ref().unwrap().value_nm, 150_000);
        assert_eq!(attributes.line_spacing, 1.0);
        assert!(attributes.keep_upright);
        assert!(fields.iter().any(|field| field.name == "KiLib_Generator"));
    }
}
