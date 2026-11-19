//! Fail-closed KiCad PCB snapshot to Specctra DSN lowering.
//!
//! The input is the exact string returned by KiCad's IPC
//! `SaveDocumentToString`; this module never reads or edits the live board
//! file. The first supported profile is intentionally narrow and rejects
//! every feature it cannot represent without approximation.

use anyhow::{bail, Context, Result};
use konnect_ipc::IpcEffectiveRoutingRules;
use konnect_sexp::{parse_sexp, SexpNode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specctra::read::{ListTokenizer, ReadDsn};
use specctra::structure as dsn;
use specctra::write::{ListWriter, WriteSes};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Cursor};
use std::path::Path;

const UM_PER_MM: f64 = 1_000.0;
const DSN_RESOLUTION: f32 = 10.0;

#[derive(Debug, Clone)]
pub(crate) struct ExportBundle {
    pub dsn: String,
    pub manifest: String,
    pub source_sha256: String,
    pub ses_importable: bool,
    pub component_count: usize,
    pub pad_count: usize,
    pub net_count: usize,
    pub class_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PadShape {
    Circle,
    Rect,
    #[serde(rename = "roundrect")]
    RoundRect,
    Oval,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PadstackKey {
    shape: PadShape,
    layers: Vec<String>,
    size_x_um: i64,
    size_y_um: i64,
    drill_um: Option<i64>,
    roundrect_size_nm: Option<(i64, i64)>,
    round_radius_nm: i64,
}

#[derive(Debug, Clone)]
struct PadModel {
    number: String,
    x_um: i64,
    y_um: i64,
    rotation_degrees: f64,
    net: Option<String>,
    padstack: PadstackKey,
}

#[derive(Debug, Clone)]
struct FootprintModel {
    reference: String,
    kiid: String,
    image_name: String,
    x_um: i64,
    y_um: i64,
    rotation_degrees: f64,
    pads: Vec<PadModel>,
}

#[derive(Debug, Clone)]
struct LockedTrackModel {
    start_x_um: i64,
    start_y_um: i64,
    end_x_um: i64,
    end_y_um: i64,
    width_um: i64,
    layer: String,
    net: String,
}

#[derive(Debug, Clone)]
struct LockedViaModel {
    x_um: i64,
    y_um: i64,
    diameter_um: i64,
    drill_um: i64,
    net: String,
}

#[derive(Debug, Clone, Default)]
struct LockedRouting {
    tracks: Vec<LockedTrackModel>,
    vias: Vec<LockedViaModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RuleKey {
    track_width_um: i64,
    clearance_um: i64,
    via_diameter_um: i64,
    via_drill_um: i64,
}

type RuleGroups = BTreeMap<RuleKey, Vec<String>>;
type NetClassNames = BTreeMap<String, String>;

#[derive(Debug, Deserialize, Serialize)]
struct Manifest {
    schema_version: u32,
    board_path: String,
    source_sha256: String,
    coordinate_unit: String,
    resolution: u32,
    supported_profile: SupportedProfile,
    layers: Vec<ManifestLayer>,
    components: Vec<ManifestComponent>,
    nets: Vec<ManifestNet>,
    padstacks: Vec<ManifestPadstack>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SupportedProfile {
    copper_layers: u32,
    component_side: String,
    pad_shapes: Vec<String>,
    existing_routing: bool,
    #[serde(default)]
    locked_track_count: usize,
    #[serde(default)]
    locked_via_count: usize,
    copper_zones: bool,
    custom_rules: bool,
    outline: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestLayer {
    kicad_name: String,
    dsn_name: String,
    index: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestComponent {
    reference: String,
    kiid: String,
    image_name: String,
    x_um: i64,
    y_um: i64,
    rotation_degrees: f64,
    side: String,
    pads: Vec<ManifestPin>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestPin {
    pad_number: String,
    dsn_pin: String,
    net: Option<String>,
    padstack_name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestNet {
    name: String,
    pins: Vec<String>,
    class_name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestPadstack {
    name: String,
    purpose: String,
    shape: PadShape,
    layers: Vec<String>,
    size_x_um: i64,
    size_y_um: i64,
    drill_um: Option<i64>,
}

pub(crate) fn export_dsn(
    board_path: &Path,
    board_source: &str,
    effective_rules: &IpcEffectiveRoutingRules,
) -> Result<ExportBundle> {
    let tree = parse_sexp(board_source).context("parse KiCad IPC board snapshot")?;
    if tree.head() != Some("kicad_pcb") {
        bail!("KiCad IPC snapshot root is not 'kicad_pcb'");
    }

    let copper_layers = copper_layers(&tree)?;
    let outline = simple_closed_outline(&tree)?;
    let net_table = top_level_net_table(&tree);
    let locked_routing = locked_routing(&tree, &net_table, &copper_layers)?;
    let footprints = footprints(&tree, &net_table)?;
    if footprints.is_empty() {
        bail!("supported routing profile requires at least one footprint");
    }

    let mut net_pins: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for footprint in &footprints {
        for pad in &footprint.pads {
            if let Some(net) = &pad.net {
                net_pins
                    .entry(net.clone())
                    .or_default()
                    .push(format!("{}-{}", footprint.reference, pad.number));
            }
        }
    }
    if net_pins.is_empty() {
        bail!("supported routing profile requires at least one connected pad");
    }
    for pins in net_pins.values_mut() {
        pins.sort();
        pins.dedup();
    }

    let (class_nets, net_classes) = normalize_rules(&net_pins, effective_rules)?;

    let mut padstack_keys = BTreeSet::new();
    for footprint in &footprints {
        for pad in &footprint.pads {
            padstack_keys.insert(pad.padstack.clone());
        }
    }
    let padstack_names: BTreeMap<PadstackKey, String> = padstack_keys
        .into_iter()
        .enumerate()
        .map(|(index, key)| (key, format!("konnect_pad_{:04}", index + 1)))
        .collect();

    let via_keys: BTreeSet<RuleKey> = class_nets.keys().cloned().collect();
    let via_names: BTreeMap<RuleKey, String> = via_keys
        .into_iter()
        .enumerate()
        .map(|(index, key)| (key, format!("konnect_via_{:04}", index + 1)))
        .collect();

    let pcb = build_pcb(
        board_path,
        &copper_layers,
        &outline,
        &footprints,
        &net_pins,
        &class_nets,
        &net_classes,
        &padstack_names,
        &via_names,
        &locked_routing,
    )?;
    let dsn = serialize_and_validate(pcb)?;
    let source_sha256 = sha256_hex(board_source.as_bytes());
    let manifest = build_manifest(
        board_path,
        &source_sha256,
        &copper_layers,
        &footprints,
        &net_pins,
        &net_classes,
        &padstack_names,
        &via_names,
        &locked_routing,
    )?;

    Ok(ExportBundle {
        dsn,
        manifest,
        source_sha256,
        ses_importable: true,
        component_count: footprints.len(),
        pad_count: footprints
            .iter()
            .map(|footprint| footprint.pads.len())
            .sum(),
        net_count: net_pins.len(),
        class_count: class_nets.len(),
    })
}

/// Bind a native DSN outside the strict SES profile to its exact live
/// snapshot. The receipt records identity and the refusal reason without
/// inventing a reverse mapping for rules or geometry outside that profile.
pub(crate) fn native_export_only(
    board_path: &Path,
    source: &str,
    dsn: String,
    reason: &str,
) -> Result<ExportBundle> {
    let board = parse_sexp(source).context("parse KiCad IPC board snapshot")?;
    if board.head() != Some("kicad_pcb") {
        bail!("KiCad IPC snapshot root is not 'kicad_pcb'");
    }
    let native = dsn_identity(&checked_native_tree(&dsn)?)?;
    let layers = declared_copper_layers(&board)?;
    if layers
        .iter()
        .enumerate()
        .any(|(index, layer)| native.layers.get(&index) != Some(layer))
        || layers.len() != native.layers.len()
    {
        bail!("native DSN changed the board's copper-layer inventory");
    }
    let mut components = BTreeMap::new();
    let mut nets = BTreeMap::<String, BTreeSet<String>>::new();
    let mut pad_count = 0;
    let net_table = top_level_net_table(&board);
    for footprint in board.find_all("footprint") {
        let reference = property_value(footprint, "Reference")
            .filter(|r| !r.is_empty())
            .context("footprint has no Reference property")?;
        let kiid = footprint
            .find_str("uuid")
            .filter(|id| !id.is_empty())
            .context("footprint has no UUID")?;
        if components.insert(reference, kiid).is_some() {
            bail!("board repeats footprint '{reference}'");
        }
        let actual = native
            .components
            .get(reference)
            .with_context(|| format!("native DSN omitted component '{reference}'"))?;
        let at = footprint.find("at").context("footprint has no position")?;
        let rotation = optional_rotation(at, "footprint rotation")?;
        let (side, flip) = match footprint.find_str("layer") {
            Some("F.Cu") => ("front", 1.0),
            Some("B.Cu") => ("back", -1.0),
            _ => bail!("footprint '{reference}' is on an unrecognized component side"),
        };
        if actual.side != side
            || !matches_dsn_position(at, actual.x, actual.y)?
            || !angles_match(rotation * flip, actual.rotation)
        {
            bail!("native DSN placement cannot be bound to footprint '{reference}'");
        }
        let mut pin_ids = BTreeSet::new();
        for pad in footprint.find_all("pad") {
            // KiCad exports mechanical holes as image keepouts, outside
            // the native signal pin inventory.
            if pad.get(2).and_then(SexpNode::as_str) == Some("np_thru_hole")
                && pad.find("net").is_none()
            {
                continue;
            }
            let number = pad
                .get(1)
                .and_then(SexpNode::as_str)
                .filter(|n| !n.is_empty())
                .context("native DSN verification requires numbered board pads")?;
            if !pin_ids.insert(number) {
                bail!("footprint '{reference}' repeats pin '{number}'");
            }
            let pin = actual
                .pins
                .get(number)
                .with_context(|| format!("native DSN omitted pin '{reference}-{number}'"))?;
            let at = pad.find("at").context("pad has no position")?;
            let pin_rotation =
                relative_pad_rotation(optional_rotation(at, "pad rotation")?, rotation);
            // KiCad's back image reverses the local Y axis and rotations.
            if !matches_dsn_position(at, pin.x, pin.y * flip)?
                || !angles_match(pin_rotation * flip, pin.rotation)
                || !native.padstacks.contains_key(&pin.padstack)
            {
                bail!("native DSN cannot bind pin '{reference}-{number}'");
            }
            if let Some(net) = pad
                .find("net")
                .and_then(|node| resolve_net(node, &net_table))
            {
                nets.entry(net)
                    .or_default()
                    .insert(format!("{reference}-{number}"));
            }
        }
        if pin_ids.len() != actual.pins.len() {
            bail!("native DSN changed pins for component '{reference}'");
        }
        pad_count += pin_ids.len();
    }
    if components.is_empty() || components.len() != native.components.len() {
        bail!("native DSN changed the component inventory");
    }
    if nets != native.nets {
        bail!("native DSN changed net or pin membership");
    }
    let source_sha256 = sha256_hex(source.as_bytes());
    let manifest = serde_json::to_string_pretty(&serde_json::json!({
        "kind": "konnect_native_dsn_receipt",
        "schema_version": 1,
        "ses_importable": false,
        "reason": reason,
        "board_path": board_path,
        "source_sha256": source_sha256,
        "dsn_sha256": sha256_hex(dsn.as_bytes()),
        "components": components,
        "layers": layers,
        "nets": nets,
    }))?;
    Ok(ExportBundle {
        dsn,
        manifest,
        source_sha256,
        ses_importable: false,
        component_count: components.len(),
        pad_count,
        net_count: nets.len(),
        class_count: native.classes.len(),
    })
}

fn matches_dsn_position(at: &SexpNode, x: f64, y: f64) -> Result<bool> {
    let source_x = finite_number(at.get_f64(1), "board x")? * UM_PER_MM;
    let source_y = finite_number(at.get_f64(2), "board y")? * UM_PER_MM;
    Ok((x - source_x).abs() <= 0.5 && (y + source_y).abs() <= 0.5)
}

/// Replace Konnect's deterministic DSN with a KiCad-native DSN while keeping
/// the same fail-closed board profile and rewriting the reverse manifest to
/// the exact identifiers KiCad emitted. This lets the strict SES importer
/// remain authoritative instead of trusting a second, looser return path.
pub(crate) fn adopt_native_dsn(
    mut baseline: ExportBundle,
    native_dsn: String,
) -> Result<ExportBundle> {
    let baseline_tree = parse_dsn_sexp(&baseline.dsn).context("parse Konnect baseline DSN")?;
    let native_tree = checked_native_tree(&native_dsn)?;
    let baseline_identity = dsn_identity(&baseline_tree)?;
    let native_identity = dsn_identity(&native_tree)?;
    let mut manifest: Manifest =
        serde_json::from_str(&baseline.manifest).context("parse baseline routing manifest")?;

    let layer_names = correlate_layers(&baseline_identity, &native_identity)?;
    let (class_names, via_names) = correlate_classes(&baseline_identity, &native_identity)?;
    let padstack_names = correlate_components(&baseline_identity, &native_identity)?;
    validate_native_nets(&baseline_identity, &native_identity)?;

    for layer in &mut manifest.layers {
        layer.dsn_name = layer_names
            .get(&layer.dsn_name)
            .with_context(|| format!("native DSN omitted layer '{}'", layer.dsn_name))?
            .clone();
    }
    for component in &mut manifest.components {
        let native = native_identity
            .components
            .get(&component.reference)
            .with_context(|| format!("native DSN omitted component '{}'", component.reference))?;
        component.image_name = native.image_name.clone();
        component.x_um = native.x.round() as i64;
        component.y_um = native.y.round() as i64;
        for pin in &mut component.pads {
            pin.padstack_name = padstack_names
                .get(&pin.padstack_name)
                .with_context(|| {
                    format!(
                        "native DSN omitted padstack mapping for {}-{}",
                        component.reference, pin.pad_number
                    )
                })?
                .clone();
        }
    }
    for net in &mut manifest.nets {
        net.class_name = class_names
            .get(&net.class_name)
            .with_context(|| format!("native DSN omitted class mapping for '{}'", net.name))?
            .clone();
    }
    for padstack in &mut manifest.padstacks {
        let mapping = if padstack.purpose == "via" {
            &via_names
        } else {
            &padstack_names
        };
        padstack.name = mapping
            .get(&padstack.name)
            .with_context(|| format!("native DSN omitted padstack '{}'", padstack.name))?
            .clone();
    }
    manifest
        .padstacks
        .sort_by(|left, right| left.name.cmp(&right.name));
    baseline.manifest =
        serde_json::to_string_pretty(&manifest).context("serialize native routing manifest")?;
    baseline.dsn = native_dsn;
    Ok(baseline)
}

#[derive(Debug, Clone)]
struct DsnPlacementIdentity {
    image_name: String,
    x: f64,
    y: f64,
    side: String,
    rotation: f64,
    pins: BTreeMap<String, DsnPinIdentity>,
}

#[derive(Debug, Clone)]
struct DsnPinIdentity {
    padstack: String,
    x: f64,
    y: f64,
    rotation: f64,
}

fn relative_pad_rotation(absolute: f64, footprint: f64) -> f64 {
    (absolute.rem_euclid(360.0) - footprint.rem_euclid(360.0)).rem_euclid(360.0)
}

fn angles_match(left: f64, right: f64) -> bool {
    const SERIALIZED_ANGLE_TOLERANCE_DEGREES: f64 = 1e-6;
    let difference = left.rem_euclid(360.0) - right.rem_euclid(360.0);
    ((difference + 180.0).rem_euclid(360.0) - 180.0).abs() <= SERIALIZED_ANGLE_TOLERANCE_DEGREES
}

#[derive(Debug, Clone)]
struct DsnClassIdentity {
    nets: BTreeSet<String>,
    via_name: String,
    width: i64,
    clearance: i64,
}

#[derive(Debug, Clone, PartialEq)]
struct DsnShapeIdentity {
    layer: String,
    geometry: DsnGeometry,
}

#[derive(Debug, Clone, PartialEq)]
enum DsnGeometry {
    Circle { diameter: f64, offset: (f64, f64) },
    Rect([f64; 4]),
    Path { width: f64, points: Vec<(f64, f64)> },
    Polygon { width: f64, points: Vec<(f64, f64)> },
}

#[derive(Debug, Clone)]
struct DsnIdentity {
    layers: BTreeMap<usize, String>,
    components: BTreeMap<String, DsnPlacementIdentity>,
    nets: BTreeMap<String, BTreeSet<String>>,
    classes: BTreeMap<String, DsnClassIdentity>,
    padstacks: BTreeMap<String, Vec<DsnShapeIdentity>>,
}

fn validate_dsn_syntax(source: &str, label: &str) -> Result<()> {
    let cursor = Cursor::new(source.as_bytes());
    let mut tokenizer = ListTokenizer::new(BufReader::new(cursor));
    dsn::DsnFile::read_dsn(&mut tokenizer)
        .map_err(|error| anyhow::anyhow!("{label} failed Specctra parser validation: {error}"))?;
    Ok(())
}

fn parse_dsn_sexp(source: &str) -> Result<SexpNode> {
    // Specctra represents the quote delimiter as the bare token
    // `(string_quote ")`. KiCad's S-expression parser correctly treats the
    // opening quote as a string delimiter, so give only this metadata token an
    // escaped representation for structural inspection. The original DSN is
    // retained byte-for-byte and independently validated by Topola's parser.
    let compatible = source.replace("(string_quote \")", "(string_quote \"\\\"\")");
    parse_sexp(&compatible).context("parse normalized Specctra S-expression")
}

fn checked_native_tree(source: &str) -> Result<SexpNode> {
    let native = parse_dsn_sexp(source).context("parse KiCad native DSN")?;
    // Topola's outline type only recognizes paths. For syntax validation
    // only, adapt KiCad's image outline polygons to equivalent closed paths.
    // The authenticated native DSN itself is returned byte-for-byte.
    let mut validation = native.clone();
    if let SexpNode::List(root) = &mut validation {
        for group in root
            .iter_mut()
            .filter(|node| node.head() == Some("library"))
        {
            let SexpNode::List(library) = group else {
                continue;
            };
            for node in library
                .iter_mut()
                .filter(|node| node.head() == Some("image"))
            {
                let SexpNode::List(image) = node else {
                    continue;
                };
                for outline in image
                    .iter_mut()
                    .filter(|node| node.head() == Some("outline"))
                {
                    let SexpNode::List(outline) = outline else {
                        continue;
                    };
                    if let Some(SexpNode::List(shape)) = outline.get_mut(1) {
                        if shape.first().and_then(SexpNode::as_str) == Some("polygon") {
                            if shape.len() < 9 || (shape.len() - 3) % 2 != 0 {
                                bail!("native DSN has a malformed polygon outline");
                            }
                            shape[0] = SexpNode::Atom("path".into());
                            if shape[3..5] != shape[shape.len() - 2..] {
                                shape.extend_from_within(3..5);
                            }
                        }
                    }
                }
            }
        }
    }
    fn write_validation(node: &SexpNode, output: &mut String) {
        match node {
            SexpNode::Atom(atom) => output.push_str(atom),
            SexpNode::Str(text) => {
                output.push_str(&serde_json::to_string(text).expect("string serialization"))
            }
            SexpNode::List(children) => {
                if node.head() == Some("string_quote")
                    && node.get(1).and_then(SexpNode::as_str) == Some("\"")
                {
                    output.push_str(r#"(string_quote ")"#);
                    return;
                }
                output.push('(');
                for (index, child) in children.iter().enumerate() {
                    if index > 0 {
                        output.push(' ');
                    }
                    write_validation(child, output);
                }
                output.push(')');
            }
        }
    }
    let mut projected = String::new();
    write_validation(&validation, &mut projected);
    validate_dsn_syntax(&projected, "KiCad native DSN")?;
    Ok(native)
}

fn dsn_shape(node: &SexpNode) -> Result<DsnShapeIdentity> {
    let kind = node.head().context("DSN padstack geometry has no kind")?;
    let layer = dsn_atom(node, 1, "pad geometry layer")?.to_string();
    let len = node.children().unwrap_or(&[]).len();
    let n = |i| dsn_number(node, i, "pad geometry coordinate");
    let coordinate_pairs = || -> Result<Vec<(f64, f64)>> {
        if !(len - 3).is_multiple_of(2) {
            bail!("DSN pad geometry has an incomplete coordinate pair");
        }
        (3..len)
            .step_by(2)
            .map(|i| Ok((n(i)?, n(i + 1)?)))
            .collect()
    };
    let geometry = match kind {
        "circle" if len == 3 || len == 5 => {
            let diameter = n(2)?;
            if diameter <= 0.0 {
                bail!("DSN circle diameter must be positive");
            }
            let offset = if len == 5 { (n(3)?, n(4)?) } else { (0.0, 0.0) };
            DsnGeometry::Circle { diameter, offset }
        }
        "rect" if len == 6 => DsnGeometry::Rect([n(2)?, n(3)?, n(4)?, n(5)?]),
        "path" if len >= 7 && n(2)? > 0.0 => DsnGeometry::Path {
            width: n(2)?,
            points: coordinate_pairs()?,
        },
        "polygon" if len >= 9 && n(2)? >= 0.0 => DsnGeometry::Polygon {
            width: n(2)?,
            points: coordinate_pairs()?,
        },
        _ => bail!("DSN padstack has malformed or unsupported shape '{kind}'"),
    };
    Ok(DsnShapeIdentity { layer, geometry })
}

/// The sampled native KiCad contour and the tangent polygon use different
/// tessellations of the same roundrect. Bound the difference for polygon
/// pairs; preserve exact checks for every other geometry and pin rotation.
fn equivalent_pad_geometry(expected: &[DsnShapeIdentity], actual: &[DsnShapeIdentity]) -> bool {
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(left, right)| {
            if left == right {
                return true;
            }
            match (&left.geometry, &right.geometry) {
                (
                    DsnGeometry::Polygon {
                        width: a,
                        points: left_points,
                    },
                    DsnGeometry::Polygon {
                        width: b,
                        points: right_points,
                    },
                ) => {
                    left.layer == right.layer
                        && a == b
                        && near_convex_polygon(left_points, right_points)
                        && near_convex_polygon(right_points, left_points)
                }
                _ => false,
            }
        })
}

fn near_convex_polygon(points: &[(f64, f64)], boundary: &[(f64, f64)]) -> bool {
    const TOLERANCE_UM: f64 = 3.0;
    let boundary = boundary.strip_suffix(&[boundary[0]]).unwrap_or(boundary);
    if boundary.len() < 3 {
        return false;
    }
    let edges = || {
        boundary
            .iter()
            .copied()
            .zip(boundary.iter().copied().cycle().skip(1))
    };
    let delta = |(a, b), (c, d)| (a - c, b - d);
    let cross = |(a, b), (c, d)| a * d - b * c;
    let mut signs = edges()
        .zip(edges().cycle().skip(1))
        .map(|((a, b), (_, c))| cross(delta(b, a), delta(c, b)))
        .filter(|v| *v != 0.0);
    let Some(first) = signs.next() else {
        return false;
    };
    if signs.any(|value| value * first < 0.0) {
        return false;
    }
    points.iter().copied().all(|point| {
        edges().all(|(a, b)| cross(delta(b, a), delta(point, a)) * first >= 0.0)
            || edges().any(|(a, b)| {
                let (dx, dy) = delta(b, a);
                let (x, y) = delta(point, a);
                let len2 = dx * dx + dy * dy;
                let t = if len2 == 0.0 {
                    0.0
                } else {
                    ((x * dx + y * dy) / len2).clamp(0.0, 1.0)
                };
                (x - t * dx).powi(2) + (y - t * dy).powi(2) <= TOLERANCE_UM.powi(2)
            })
    })
}

fn dsn_identity(root: &SexpNode) -> Result<DsnIdentity> {
    if root.head() != Some("pcb") {
        bail!("Specctra export root is not 'pcb'");
    }
    let resolution = root.find("resolution").context("DSN has no resolution")?;
    if resolution.get(1).and_then(SexpNode::as_str) != Some("um")
        || resolution.get_f64(2) != Some(DSN_RESOLUTION as f64)
        || root.find_str("unit") != Some("um")
    {
        bail!("DSN does not use the supported micrometre coordinate system");
    }
    let structure = root.find("structure").context("DSN has no structure")?;
    let mut layers = BTreeMap::new();
    for layer in structure.find_all("layer") {
        let name = dsn_atom(layer, 1, "layer name")?.to_string();
        let index = layer
            .find("property")
            .and_then(|property| property.find("index"))
            .and_then(|index| index.get(1))
            .and_then(SexpNode::as_str)
            .context("DSN layer has no index")?
            .parse::<usize>()
            .context("DSN layer index is not an integer")?;
        if layers.insert(index, name).is_some() {
            bail!("DSN repeats a layer index");
        }
    }

    let library = root.find("library").context("DSN has no library")?;
    let mut image_pins = BTreeMap::<String, BTreeMap<String, DsnPinIdentity>>::new();
    for image in library.find_all("image") {
        let name = dsn_atom(image, 1, "image name")?.to_string();
        let mut pins = BTreeMap::new();
        for pin in image.find_all("pin") {
            let padstack = dsn_atom(pin, 1, "pin padstack")?.to_string();
            let (number_index, rotation) = match pin.get(2) {
                Some(rotate) if rotate.head() == Some("rotate") => {
                    (3, dsn_number(rotate, 1, "pin rotation")?)
                }
                _ => (2, 0.0),
            };
            let number = dsn_atom(pin, number_index, "pin number")?.to_string();
            let identity = DsnPinIdentity {
                padstack,
                x: dsn_number(pin, number_index + 1, "pin x")?,
                y: dsn_number(pin, number_index + 2, "pin y")?,
                rotation,
            };
            if pins.insert(number, identity).is_some() {
                bail!("DSN image '{name}' repeats a pin number");
            }
        }
        if image_pins.insert(name.clone(), pins).is_some() {
            bail!("DSN repeats image '{name}'");
        }
    }
    let mut padstacks = BTreeMap::new();
    for padstack in library.find_all("padstack") {
        let name = dsn_atom(padstack, 1, "padstack name")?.to_string();
        let shapes = padstack
            .find_all("shape")
            .into_iter()
            .map(|shape| {
                let geometry = shape.get(1).context("DSN shape has no geometry")?;
                dsn_shape(geometry)
            })
            .collect::<Result<Vec<_>>>()?;
        if shapes.is_empty() || padstacks.insert(name.clone(), shapes).is_some() {
            bail!("DSN contains an empty or duplicate padstack '{name}'");
        }
    }

    let placement = root.find("placement").context("DSN has no placement")?;
    let mut components = BTreeMap::new();
    for component in placement.find_all("component") {
        let image_name = dsn_atom(component, 1, "component image")?.to_string();
        let pins = image_pins
            .get(&image_name)
            .with_context(|| format!("DSN placement uses unknown image '{image_name}'"))?
            .clone();
        for place in component.find_all("place") {
            let reference = dsn_atom(place, 1, "place reference")?.to_string();
            let x = dsn_number(place, 2, "place x")?;
            let y = dsn_number(place, 3, "place y")?;
            let side = dsn_atom(place, 4, "place side")?.to_string();
            let rotation = dsn_number(place, 5, "place rotation")?;
            let identity = DsnPlacementIdentity {
                image_name: image_name.clone(),
                x,
                y,
                side,
                rotation,
                pins: pins.clone(),
            };
            if components.insert(reference.clone(), identity).is_some() {
                bail!("DSN repeats component '{reference}'");
            }
        }
    }

    let network = root.find("network").context("DSN has no network")?;
    let mut nets = BTreeMap::new();
    for net in network.find_all("net") {
        let name = dsn_atom(net, 1, "net name")?.to_string();
        let pins = net
            .find("pins")
            .context("DSN net has no pins")?
            .children()
            .context("DSN pins is not a list")?
            .iter()
            .skip(1)
            .map(|pin| {
                pin.as_str()
                    .map(str::to_string)
                    .context("DSN pins contains a list")
            })
            .collect::<Result<BTreeSet<_>>>()?;
        if pins.is_empty() || nets.insert(name.clone(), pins).is_some() {
            bail!("DSN contains an empty or duplicate net '{name}'");
        }
    }
    let mut classes = BTreeMap::new();
    for class in network.find_all("class") {
        let name = dsn_atom(class, 1, "class name")?.to_string();
        let children = class.children().context("DSN class is not a list")?;
        let nets = children
            .iter()
            .skip(2)
            .take_while(|child| child.as_str().is_some())
            .map(|child| child.as_str().unwrap().to_string())
            .collect::<BTreeSet<_>>();
        let via_name = class
            .find("circuit")
            .and_then(|circuit| circuit.find("use_via"))
            .and_then(|via| via.get(1))
            .and_then(SexpNode::as_str)
            .context("DSN class has no use_via")?
            .to_string();
        let rule = class.find("rule").context("DSN class has no rule")?;
        let width = rule.find("width").context("DSN class has no width")?;
        let clearance = rule
            .find("clearance")
            .context("DSN class has no clearance")?;
        let width = dsn_integer(width, 1, "class width")?;
        let clearance = dsn_integer(clearance, 1, "class clearance")?;
        if nets.is_empty()
            || !nets.iter().all(|net| {
                network
                    .find_all("net")
                    .iter()
                    .any(|node| node.get(1).and_then(SexpNode::as_str) == Some(net))
            })
            || classes
                .insert(
                    name.clone(),
                    DsnClassIdentity {
                        nets,
                        via_name,
                        width,
                        clearance,
                    },
                )
                .is_some()
        {
            bail!("DSN contains an invalid or duplicate class '{name}'");
        }
    }
    Ok(DsnIdentity {
        layers,
        components,
        nets,
        classes,
        padstacks,
    })
}

fn correlate_layers(
    baseline: &DsnIdentity,
    native: &DsnIdentity,
) -> Result<BTreeMap<String, String>> {
    if baseline.layers.len() != native.layers.len() {
        bail!("native DSN changed the copper-layer count");
    }
    let mappings = baseline
        .layers
        .iter()
        .map(|(index, baseline_name)| {
            let native_name = native
                .layers
                .get(index)
                .with_context(|| format!("native DSN omitted layer index {index}"))?;
            Ok((baseline_name.clone(), native_name.clone()))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    if mappings.iter().any(|(baseline, native)| baseline != native) {
        bail!("native DSN renamed a KiCad copper layer");
    }
    Ok(mappings)
}

fn correlate_components(
    baseline: &DsnIdentity,
    native: &DsnIdentity,
) -> Result<BTreeMap<String, String>> {
    if baseline.components.len() != native.components.len() {
        bail!("native DSN changed the component count");
    }
    let mut padstacks = BTreeMap::new();
    for (reference, baseline_component) in &baseline.components {
        let native_component = native
            .components
            .get(reference)
            .with_context(|| format!("native DSN omitted component '{reference}'"))?;
        if (baseline_component.x - native_component.x).abs() > 0.5
            || (baseline_component.y - native_component.y).abs() > 0.5
            || baseline_component.side != native_component.side
            || !angles_match(baseline_component.rotation, native_component.rotation)
            || baseline_component.pins.keys().collect::<Vec<_>>()
                != native_component.pins.keys().collect::<Vec<_>>()
        {
            bail!("native DSN changed placement or pins for component '{reference}'");
        }
        for (pin, baseline_pin) in &baseline_component.pins {
            let native_pin = native_component.pins.get(pin).unwrap();
            if (baseline_pin.x - native_pin.x).abs() > 0.5
                || (baseline_pin.y - native_pin.y).abs() > 0.5
            {
                bail!("native DSN changed pin position for component '{reference}' pin '{pin}'");
            }
            if !angles_match(baseline_pin.rotation, native_pin.rotation) {
                bail!("native DSN changed pin rotation for component '{reference}' pin '{pin}'");
            }
            let baseline_shape = baseline
                .padstacks
                .get(&baseline_pin.padstack)
                .with_context(|| {
                    format!("baseline DSN omitted padstack '{}'", baseline_pin.padstack)
                })?;
            let native_shape = native
                .padstacks
                .get(&native_pin.padstack)
                .with_context(|| {
                    format!("native DSN omitted padstack '{}'", native_pin.padstack)
                })?;
            if !equivalent_pad_geometry(baseline_shape, native_shape) {
                bail!("native DSN changed pad geometry for component '{reference}' pin '{pin}'");
            }
            if padstacks
                .insert(baseline_pin.padstack.clone(), native_pin.padstack.clone())
                .is_some_and(|previous| previous != native_pin.padstack)
            {
                bail!("native DSN maps one pad geometry to inconsistent padstacks");
            }
        }
    }
    Ok(padstacks)
}

fn validate_native_nets(baseline: &DsnIdentity, native: &DsnIdentity) -> Result<()> {
    if baseline.nets != native.nets {
        bail!("native DSN changed net or pin membership");
    }
    Ok(())
}

fn correlate_classes(
    baseline: &DsnIdentity,
    native: &DsnIdentity,
) -> Result<(BTreeMap<String, String>, BTreeMap<String, String>)> {
    if baseline.classes.len() != native.classes.len() {
        bail!("native DSN changed the routing-class count");
    }
    let mut class_names = BTreeMap::new();
    let mut via_names = BTreeMap::new();
    for (baseline_name, baseline_class) in &baseline.classes {
        let matches = native
            .classes
            .iter()
            .filter(|(_, native_class)| native_class.nets == baseline_class.nets)
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            bail!("native DSN has no unique class for '{baseline_name}'");
        }
        let (native_name, native_class) = matches[0];
        if baseline_class.width != native_class.width
            || baseline_class.clearance != native_class.clearance
        {
            bail!("native DSN changed width or clearance for class '{baseline_name}'");
        }
        let baseline_via = baseline
            .padstacks
            .get(&baseline_class.via_name)
            .with_context(|| format!("baseline DSN omitted via '{}'", baseline_class.via_name))?;
        let native_via = native
            .padstacks
            .get(&native_class.via_name)
            .with_context(|| format!("native DSN omitted via '{}'", native_class.via_name))?;
        if baseline_via != native_via {
            bail!("native DSN changed via geometry for class '{baseline_name}'");
        }
        class_names.insert(baseline_name.clone(), native_name.clone());
        if via_names
            .insert(
                baseline_class.via_name.clone(),
                native_class.via_name.clone(),
            )
            .is_some_and(|previous| previous != native_class.via_name)
        {
            bail!("native DSN maps one via rule to inconsistent padstacks");
        }
    }
    Ok((class_names, via_names))
}

fn dsn_atom<'a>(node: &'a SexpNode, index: usize, label: &str) -> Result<&'a str> {
    node.get(index)
        .and_then(SexpNode::as_str)
        .with_context(|| format!("DSN is missing {label}"))
}

fn dsn_number(node: &SexpNode, index: usize, label: &str) -> Result<f64> {
    let value = dsn_atom(node, index, label)?
        .parse::<f64>()
        .with_context(|| format!("DSN {label} is not numeric"))?;
    if !value.is_finite() {
        bail!("DSN {label} is not finite");
    }
    Ok(value)
}

fn dsn_integer(node: &SexpNode, index: usize, label: &str) -> Result<i64> {
    let value = dsn_number(node, index, label)?;
    if value.fract() != 0.0 || value < i64::MIN as f64 || value > i64::MAX as f64 {
        bail!("DSN {label} is not a supported integer");
    }
    Ok(value as i64)
}

fn locked_routing(
    tree: &SexpNode,
    net_table: &BTreeMap<String, String>,
    copper_layers: &[String],
) -> Result<LockedRouting> {
    let zone_count = tree.find_all("zone").len();
    if zone_count > 0 {
        bail!(
            "unsupported first routing profile: board contains {zone_count} copper zone or rule area(s)"
        );
    }
    let arc_count = tree.find_all("arc").len();
    if arc_count > 0 {
        let locked = tree
            .find_all("arc")
            .iter()
            .filter(|arc| arc.find_str("locked") == Some("yes"))
            .count();
        if locked > 0 {
            bail!(
                "unsupported first routing profile: board contains {locked} locked routed arc(s), which cannot be represented without approximation"
            );
        }
        bail!(
            "unsupported first routing profile: board contains {arc_count} existing routed arc(s)"
        );
    }

    let mut routing = LockedRouting::default();
    for segment in tree.find_all("segment") {
        if segment.find_str("locked") != Some("yes") {
            bail!("unsupported first routing profile: board contains an existing track segment that is not locked");
        }
        let layer = segment
            .find_str("layer")
            .context("locked track segment has no layer")?
            .to_string();
        if !copper_layers.contains(&layer) {
            bail!("locked track segment uses unsupported layer '{layer}'");
        }
        let net = segment
            .find("net")
            .and_then(|node| resolve_net(node, net_table))
            .context("locked track segment has no connected net")?;
        let (start_x_um, start_y_um) = point_um(segment, "start")?;
        let (end_x_um, end_y_um) = point_um(segment, "end")?;
        if start_x_um == end_x_um && start_y_um == end_y_um {
            bail!("locked track segment has zero length");
        }
        routing.tracks.push(LockedTrackModel {
            start_x_um,
            start_y_um,
            end_x_um,
            end_y_um,
            width_um: positive_um(
                segment.find("width").and_then(|width| width.get_f64(1)),
                "locked track width",
            )?,
            layer,
            net,
        });
    }
    for via in tree.find_all("via") {
        if via.find_str("locked") != Some("yes") {
            bail!("unsupported first routing profile: board contains an existing via that is not locked");
        }
        if let Some(kind) = via.find_str("type") {
            bail!("unsupported locked via type '{kind}'; only through vias are supported");
        }
        let layers = via
            .find("layers")
            .and_then(SexpNode::children)
            .unwrap_or(&[])
            .iter()
            .skip(1)
            .filter_map(SexpNode::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        if layers != copper_layers {
            bail!(
                "unsupported locked via layer span [{}]; expected [{}]",
                layers.join(", "),
                copper_layers.join(", ")
            );
        }
        let net = via
            .find("net")
            .and_then(|node| resolve_net(node, net_table))
            .context("locked via has no connected net")?;
        let (x_um, y_um) = point_um(via, "at")?;
        let diameter_um = positive_um(
            via.find("size").and_then(|size| size.get_f64(1)),
            "locked via diameter",
        )?;
        let drill = via.find("drill").context("locked via has no drill")?;
        if drill.get(1).and_then(SexpNode::as_str) == Some("oval") {
            bail!("unsupported locked via with oval drill");
        }
        let drill_um = positive_um(drill.get_f64(1), "locked via drill")?;
        if drill_um >= diameter_um {
            bail!("locked via drill is not smaller than its diameter");
        }
        routing.vias.push(LockedViaModel {
            x_um,
            y_um,
            diameter_um,
            drill_um,
            net,
        });
    }
    routing.tracks.sort_by(|left, right| {
        (
            &left.net,
            &left.layer,
            left.start_x_um,
            left.start_y_um,
            left.end_x_um,
            left.end_y_um,
        )
            .cmp(&(
                &right.net,
                &right.layer,
                right.start_x_um,
                right.start_y_um,
                right.end_x_um,
                right.end_y_um,
            ))
    });
    routing.vias.sort_by(|left, right| {
        (&left.net, left.x_um, left.y_um).cmp(&(&right.net, right.x_um, right.y_um))
    });
    Ok(routing)
}

fn declared_copper_layers(tree: &SexpNode) -> Result<Vec<String>> {
    let layers = tree.find("layers").context("board has no layers table")?;
    let mut copper = layers
        .children()
        .unwrap_or(&[])
        .iter()
        .skip(1)
        // Layer table rows are `(numeric-id "name" type)`, so the layer name
        // is the first data item even though the numeric id is the list head.
        .filter_map(|layer| layer.get(1).and_then(SexpNode::as_str))
        .filter(|name| name.ends_with(".Cu"))
        .map(str::to_string)
        .collect::<Vec<_>>();
    copper.dedup();
    Ok(copper)
}

fn copper_layers(tree: &SexpNode) -> Result<Vec<String>> {
    let copper = declared_copper_layers(tree)?;
    if copper != ["F.Cu".to_string(), "B.Cu".to_string()] {
        bail!(
            "unsupported first routing profile: expected exactly F.Cu and B.Cu, got [{}]",
            copper.join(", ")
        );
    }
    Ok(copper)
}

fn simple_closed_outline(tree: &SexpNode) -> Result<Vec<(i64, i64)>> {
    let mut edges = Vec::new();
    for node in tree.children().unwrap_or(&[]) {
        if node.find_str("layer") != Some("Edge.Cuts") {
            continue;
        }
        match node.head() {
            Some("gr_line") => {
                let start = point_um(node, "start").context("Edge.Cuts line has no start")?;
                let end = point_um(node, "end").context("Edge.Cuts line has no end")?;
                if start == end {
                    bail!("unsupported outline: zero-length Edge.Cuts line");
                }
                edges.push((start, end));
            }
            Some(tag) if tag.starts_with("gr_") => {
                bail!("unsupported first routing profile: Edge.Cuts '{tag}' is not a straight line")
            }
            _ => {}
        }
    }
    if edges.len() < 3 {
        bail!("unsupported outline: need at least three Edge.Cuts lines");
    }

    let mut adjacency: BTreeMap<(i64, i64), Vec<(i64, i64)>> = BTreeMap::new();
    for (start, end) in &edges {
        adjacency.entry(*start).or_default().push(*end);
        adjacency.entry(*end).or_default().push(*start);
    }
    for (point, neighbours) in &mut adjacency {
        neighbours.sort();
        neighbours.dedup();
        if neighbours.len() != 2 {
            bail!(
                "unsupported outline: vertex ({}, {}) has degree {}, expected 2",
                point.0,
                point.1,
                neighbours.len()
            );
        }
    }

    let start = *adjacency.keys().next().context("outline has no vertices")?;
    let mut ordered = vec![start];
    let mut previous = None;
    let mut current = start;
    loop {
        let neighbours = &adjacency[&current];
        let next = match previous {
            None => neighbours[0],
            Some(previous) if neighbours[0] == previous => neighbours[1],
            Some(_) => neighbours[0],
        };
        if next == start {
            ordered.push(start);
            break;
        }
        if ordered.contains(&next) {
            bail!("unsupported outline: Edge.Cuts contains more than one loop");
        }
        ordered.push(next);
        previous = Some(current);
        current = next;
    }
    if ordered.len() != edges.len() + 1 {
        bail!("unsupported outline: Edge.Cuts is not one closed loop");
    }
    Ok(ordered)
}

fn footprints(
    tree: &SexpNode,
    net_table: &BTreeMap<String, String>,
) -> Result<Vec<FootprintModel>> {
    let mut output = Vec::new();
    let mut references = BTreeSet::new();
    for footprint in tree.find_all("footprint") {
        let layer = footprint
            .find_str("layer")
            .context("footprint has no layer")?;
        if layer != "F.Cu" {
            bail!("unsupported first routing profile: footprint on '{layer}'");
        }
        if footprint.find("clearance").is_some() {
            bail!("unsupported first routing profile: footprint has a local clearance override");
        }
        let reference = property_value(footprint, "Reference")
            .filter(|value| !value.is_empty())
            .context("footprint has no Reference property")?
            .to_string();
        if !references.insert(reference.clone()) {
            bail!("duplicate footprint reference '{reference}'");
        }
        let kiid = footprint
            .find_str("uuid")
            .context("footprint has no UUID")?
            .to_string();
        let at = footprint.find("at").context("footprint has no position")?;
        let x_um = finite_um(at.get_f64(1), "footprint x")?;
        let y_um = -finite_um(at.get_f64(2), "footprint y")?;
        let rotation_degrees = optional_rotation(at, "footprint rotation")?;
        let image_name = format!("konnect_image_{reference}");

        let mut pads = Vec::new();
        let mut pad_numbers = BTreeSet::new();
        for pad in footprint.find_all("pad") {
            let number = pad
                .get(1)
                .and_then(SexpNode::as_str)
                .filter(|number| !number.is_empty())
                .context("footprint contains an unnumbered pad")?
                .to_string();
            if !pad_numbers.insert(number.clone()) {
                bail!("footprint '{reference}' has duplicate pad number '{number}'");
            }
            let pad_type = pad
                .get(2)
                .and_then(SexpNode::as_str)
                .context("pad has no type")?;
            if !matches!(pad_type, "smd" | "thru_hole") {
                bail!(
                    "unsupported first routing profile: pad {reference}-{number} has type '{pad_type}'"
                );
            }
            let shape = match pad.get(3).and_then(SexpNode::as_str) {
                Some("circle") => PadShape::Circle,
                Some("rect") => PadShape::Rect,
                Some("roundrect") => PadShape::RoundRect,
                Some("oval") => PadShape::Oval,
                Some(other) => bail!(
                    "unsupported first routing profile: pad {reference}-{number} has shape '{other}'"
                ),
                None => bail!("pad {reference}-{number} has no shape"),
            };
            if pad.find("padstack").is_some_and(|stack| {
                stack.find_str("mode") != Some("normal")
                    || stack.children().is_none_or(|children| children.len() != 2)
            }) {
                bail!("unsupported first routing profile: pad {reference}-{number} has a per-layer or unrecognized padstack");
            }
            let at = pad.find("at").context("pad has no position")?;
            let x_um = finite_um(at.get_f64(1), "pad x")?;
            let y_um = -finite_um(at.get_f64(2), "pad y")?;
            let absolute_rotation = optional_rotation(at, "pad rotation")?;
            let rotation_degrees = relative_pad_rotation(absolute_rotation, rotation_degrees);
            let size = pad.find("size").context("pad has no size")?;
            let size_x_um = positive_um(size.get_f64(1), "pad width")?;
            let size_y_um = positive_um(size.get_f64(2), "pad height")?;
            let (roundrect_size_nm, round_radius_nm) = if shape == PadShape::RoundRect {
                let width_nm = roundrect_dimension_nm(size.get_f64(1), "pad width")?;
                let height_nm = roundrect_dimension_nm(size.get_f64(2), "pad height")?;
                let ratio =
                    finite_number(pad.find_f64("roundrect_rratio"), "roundrect radius ratio")
                        .with_context(|| format!("pad {reference}-{number}"))?;
                if !(0.0..=0.5).contains(&ratio) {
                    bail!("pad {reference}-{number} roundrect radius ratio is outside 0..=0.5");
                }
                let radius_nm = (ratio * width_nm.min(height_nm) as f64).floor() as i64;
                (Some((width_nm, height_nm)), radius_nm)
            } else {
                (None, 0)
            };
            if shape == PadShape::Circle && size_x_um != size_y_um {
                bail!("circle pad {reference}-{number} does not have equal X/Y size");
            }
            if pad.find("clearance").is_some() {
                bail!(
                    "unsupported first routing profile: pad {reference}-{number} has a local clearance override"
                );
            }
            if pad.find_str("remove_unused_layers") == Some("yes") {
                bail!(
                    "unsupported first routing profile: pad {reference}-{number} removes copper on unused layers"
                );
            }
            if let Some(offset) = pad.find("offset") {
                let offset_x_um = finite_um(offset.get_f64(1), "pad offset x")?;
                let offset_y_um = finite_um(offset.get_f64(2), "pad offset y")?;
                if offset_x_um != 0 || offset_y_um != 0 {
                    bail!(
                        "unsupported first routing profile: pad {reference}-{number} has a non-zero shape offset"
                    );
                }
            }
            let layers = pad
                .find("layers")
                .and_then(SexpNode::children)
                .unwrap_or(&[])
                .iter()
                .skip(1)
                .filter_map(SexpNode::as_str)
                .filter(|layer| layer.ends_with(".Cu") || *layer == "*.Cu")
                .map(str::to_string)
                .collect::<Vec<_>>();
            let (layers, drill_um) = if pad_type == "smd" {
                if layers != ["F.Cu".to_string()] {
                    bail!(
                        "unsupported SMD pad {reference}-{number}: copper layers are [{}]",
                        layers.join(", ")
                    );
                }
                (layers, None)
            } else {
                if !(layers == ["*.Cu".to_string()]
                    || layers == ["F.Cu".to_string(), "B.Cu".to_string()])
                {
                    bail!(
                        "unsupported through-hole pad {reference}-{number}: copper layers are [{}]",
                        layers.join(", ")
                    );
                }
                let drill = pad.find("drill").context("through-hole pad has no drill")?;
                if drill.get(1).and_then(SexpNode::as_str) == Some("oval") {
                    bail!("unsupported through-hole pad {reference}-{number}: oval drill");
                }
                let drill_um = positive_um(drill.get_f64(1), "pad drill")?;
                if drill_um >= size_x_um.min(size_y_um) {
                    bail!(
                        "pad {reference}-{number} drill {drill_um} um is not smaller than its copper size"
                    );
                }
                (vec!["F.Cu".to_string(), "B.Cu".to_string()], Some(drill_um))
            };
            let net = pad
                .find("net")
                .and_then(|node| resolve_net(node, net_table));
            pads.push(PadModel {
                number,
                x_um,
                y_um,
                rotation_degrees,
                net,
                padstack: PadstackKey {
                    shape,
                    layers,
                    size_x_um,
                    size_y_um,
                    drill_um,
                    roundrect_size_nm,
                    round_radius_nm,
                },
            });
        }
        if pads.is_empty() {
            bail!("footprint '{reference}' has no pads");
        }
        pads.sort_by(|left, right| left.number.cmp(&right.number));
        output.push(FootprintModel {
            reference,
            kiid,
            image_name,
            x_um,
            y_um,
            rotation_degrees,
            pads,
        });
    }
    output.sort_by(|left, right| left.reference.cmp(&right.reference));
    Ok(output)
}

fn normalize_rules(
    net_pins: &BTreeMap<String, Vec<String>>,
    effective_rules: &IpcEffectiveRoutingRules,
) -> Result<(RuleGroups, NetClassNames)> {
    let mut class_nets: BTreeMap<RuleKey, Vec<String>> = BTreeMap::new();
    for net in net_pins.keys() {
        let rules = effective_rules.get(net).with_context(|| {
            format!("KiCad returned no effective routing rules for net '{net}'")
        })?;
        let key = RuleKey {
            track_width_um: positive_um(rules.track_width_mm, "track width")?,
            clearance_um: non_negative_um(rules.clearance_mm, "clearance")?,
            via_diameter_um: positive_um(rules.via_diameter_mm, "via diameter")?,
            via_drill_um: positive_um(rules.via_drill_mm, "via drill")?,
        };
        if key.via_drill_um >= key.via_diameter_um {
            bail!(
                "net '{net}' has via drill {} um not smaller than diameter {} um",
                key.via_drill_um,
                key.via_diameter_um
            );
        }
        class_nets.entry(key).or_default().push(net.clone());
    }
    for nets in class_nets.values_mut() {
        nets.sort();
    }
    let mut net_classes = BTreeMap::new();
    for (index, nets) in class_nets.values().enumerate() {
        let name = format!("konnect_class_{:04}", index + 1);
        for net in nets {
            net_classes.insert(net.clone(), name.clone());
        }
    }
    Ok((class_nets, net_classes))
}

#[allow(clippy::too_many_arguments)]
fn build_pcb(
    board_path: &Path,
    copper_layers: &[String],
    outline: &[(i64, i64)],
    footprints: &[FootprintModel],
    net_pins: &BTreeMap<String, Vec<String>>,
    class_nets: &BTreeMap<RuleKey, Vec<String>>,
    net_classes: &BTreeMap<String, String>,
    padstack_names: &BTreeMap<PadstackKey, String>,
    via_names: &BTreeMap<RuleKey, String>,
    locked_routing: &LockedRouting,
) -> Result<dsn::Pcb> {
    let default_rule = class_nets
        .keys()
        .next()
        .context("routing model has no rule class")?;
    let layers = copper_layers
        .iter()
        .enumerate()
        .map(|(index, name)| dsn::Layer {
            name: name.clone(),
            r#type: "signal".to_string(),
            property: Some(dsn::Property { index }),
        })
        .collect();
    let boundary = dsn::Boundary::Path(dsn::Path {
        layer: "pcb".to_string(),
        width: 0.0,
        coords: outline
            .iter()
            .map(|(x, y)| dsn::Point {
                x: *x as f64,
                y: *y as f64,
            })
            .collect(),
    });

    let components = footprints
        .iter()
        .map(|footprint| dsn::Component {
            name: footprint.image_name.clone(),
            places: vec![dsn::Place {
                name: footprint.reference.clone(),
                x: footprint.x_um as f64,
                y: footprint.y_um as f64,
                side: "front".to_string(),
                rotation: footprint.rotation_degrees,
                PN: None,
            }],
        })
        .collect();
    let images = footprints
        .iter()
        .map(|footprint| dsn::Image {
            name: footprint.image_name.clone(),
            outlines: Vec::new(),
            pins: footprint
                .pads
                .iter()
                .map(|pad| dsn::Pin {
                    name: padstack_names[&pad.padstack].clone(),
                    rotate: (pad.rotation_degrees != 0.0).then_some(pad.rotation_degrees),
                    id: pad.number.clone(),
                    x: pad.x_um as f64,
                    y: pad.y_um as f64,
                })
                .collect(),
            keepouts: dsn::Keepouts(Vec::new()),
        })
        .collect();

    let mut padstacks = padstack_names
        .iter()
        .map(|(key, name)| padstack(name, key))
        .collect::<Result<Vec<_>>>()?;
    padstacks.extend(via_names.iter().map(|(rule, name)| {
        dsn::Padstack {
            name: name.clone(),
            shapes: copper_layers
                .iter()
                .map(|layer| {
                    dsn::Shape::Circle(dsn::Circle {
                        layer: layer.clone(),
                        diameter: rule.via_diameter_um as f64,
                        offset: None,
                    })
                })
                .collect(),
            attach: Some(false),
        }
    }));
    padstacks.sort_by(|left, right| left.name.cmp(&right.name));

    let nets = net_pins
        .iter()
        .map(|(name, pins)| dsn::NetPinAssignments {
            name: name.clone(),
            pins: Some(dsn::Pins {
                names: pins.clone(),
            }),
        })
        .collect();
    let classes = class_nets
        .iter()
        .enumerate()
        .map(|(index, (rule, nets))| dsn::Class {
            name: format!("konnect_class_{:04}", index + 1),
            nets: nets.clone(),
            circuit: dsn::Circuit {
                use_via: via_names[rule].clone(),
            },
            rule: dsn::Rule {
                width: rule.track_width_um as f32,
                clearances: vec![dsn::Clearance {
                    value: rule.clearance_um as f32,
                    r#type: None,
                }],
            },
        })
        .collect();
    debug_assert_eq!(net_classes.len(), net_pins.len());

    let net_rules = class_nets
        .iter()
        .flat_map(|(rule, nets)| nets.iter().map(move |net| (net, rule)))
        .collect::<BTreeMap<_, _>>();
    let wires = locked_routing
        .tracks
        .iter()
        .map(|track| {
            if !net_rules.contains_key(&track.net) {
                bail!("locked track uses unknown routing net '{}'", track.net);
            }
            Ok(dsn::Wire {
                path: dsn::Path {
                    layer: track.layer.clone(),
                    width: track.width_um as f64,
                    coords: vec![
                        dsn::Point {
                            x: track.start_x_um as f64,
                            y: track.start_y_um as f64,
                        },
                        dsn::Point {
                            x: track.end_x_um as f64,
                            y: track.end_y_um as f64,
                        },
                    ],
                },
                net: track.net.clone(),
                r#type: "fix".to_string(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let fixed_vias = locked_routing
        .vias
        .iter()
        .map(|via| {
            let rule = net_rules
                .get(&via.net)
                .with_context(|| format!("locked via uses unknown routing net '{}'", via.net))?;
            if via.diameter_um != rule.via_diameter_um || via.drill_um != rule.via_drill_um {
                bail!(
                    "locked via on net '{}' is {}:{} um, but its effective via rule is {}:{} um",
                    via.net,
                    via.diameter_um,
                    via.drill_um,
                    rule.via_diameter_um,
                    rule.via_drill_um
                );
            }
            Ok(dsn::Via {
                name: via_names[*rule].clone(),
                x: via.x_um as f64,
                y: via.y_um as f64,
                net: via.net.clone(),
                r#type: Some("fix".to_string()),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(dsn::Pcb {
        name: board_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("board.kicad_pcb")
            .to_string(),
        parser: Some(dsn::Parser {
            string_quote: Some('"'),
            space_in_quoted_tokens: Some(true),
            host_cad: Some("Konnect".to_string()),
            host_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
        resolution: dsn::Resolution {
            unit: "um".to_string(),
            value: DSN_RESOLUTION,
        },
        unit: Some("um".to_string()),
        structure: dsn::Structure {
            layers,
            boundary,
            place_boundary: None,
            planes: Vec::new(),
            keepouts: dsn::Keepouts(Vec::new()),
            via: dsn::ViaNames {
                names: via_names.values().cloned().collect(),
            },
            grids: Vec::new(),
            rules: vec![dsn::StructureRule {
                width: Some(default_rule.track_width_um as f32),
                clearances: vec![dsn::Clearance {
                    value: default_rule.clearance_um as f32,
                    r#type: None,
                }],
            }],
        },
        placement: dsn::Placement { components },
        library: dsn::Library { images, padstacks },
        network: dsn::Network { nets, classes },
        wiring: dsn::Wiring {
            wires,
            vias: fixed_vias,
        },
    })
}

fn padstack(name: &str, key: &PadstackKey) -> Result<dsn::Padstack> {
    let (width, height) = key.roundrect_size_nm.map_or(
        (key.size_x_um as f64, key.size_y_um as f64),
        |(width_nm, height_nm)| (width_nm as f64 / 1000.0, height_nm as f64 / 1000.0),
    );
    let shapes = key
        .layers
        .iter()
        .map(|layer| {
            let circle = || {
                dsn::Shape::Circle(dsn::Circle {
                    layer: layer.clone(),
                    diameter: width,
                    offset: None,
                })
            };
            let rect = || {
                dsn::Shape::Rect(dsn::Rect {
                    layer: layer.clone(),
                    x1: -width / 2.0,
                    y1: -height / 2.0,
                    x2: width / 2.0,
                    y2: height / 2.0,
                })
            };
            Ok(match key.shape {
                PadShape::Circle => circle(),
                PadShape::Rect => rect(),
                PadShape::RoundRect if key.round_radius_nm == 0 => rect(),
                PadShape::RoundRect => dsn::Shape::Polygon(dsn::Polygon {
                    layer: layer.clone(),
                    width: 0.0,
                    coords: rounded_pad_outline(width, height, key.round_radius_nm as f64 / 1000.0)
                        .with_context(|| format!("padstack '{name}'"))?,
                }),
                PadShape::Oval if width == height => circle(),
                PadShape::Oval => {
                    let d = (width - height).abs() / 2.0;
                    let (x, y) = if width > height { (d, 0.0) } else { (0.0, d) };
                    dsn::Shape::Path(dsn::Path {
                        layer: layer.clone(),
                        width: width.min(height),
                        coords: vec![dsn::Point { x: -x, y: -y }, dsn::Point { x, y }],
                    })
                }
            })
        })
        .collect::<Result<_>>()?;
    Ok(dsn::Padstack {
        name: name.to_string(),
        shapes,
        attach: Some(false),
    })
}

/// A tangent polygon contains the true roundrect copper. The circumradius
/// error is bounded by 0.5 µm, plus at most 1 nm from flooring the radius
/// against the original KiCad dimensions.
fn rounded_pad_outline(width: f64, height: f64, radius: f64) -> Result<Vec<dsn::Point>> {
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};

    let steps = (FRAC_PI_4 / (radius / (radius + 0.5)).acos()).ceil() as usize;
    if !(1..=1024).contains(&steps) {
        bail!("roundrect radius exceeds the supported tangent-polygon budget");
    }
    let step = FRAC_PI_2 / steps as f64;
    let tangent_radius = radius / (step / 2.0).cos();
    let dx = width / 2.0 - radius;
    let dy = height / 2.0 - radius;
    let mut coords = Vec::with_capacity(4 * steps + 1);
    for (quadrant, cx, cy) in [(0, dx, dy), (1, -dx, dy), (2, -dx, -dy), (3, dx, -dy)] {
        for i in 0..steps {
            let a = quadrant as f64 * FRAC_PI_2 + (i as f64 + 0.5) * step;
            coords.push(dsn::Point {
                x: cx + tangent_radius * a.cos(),
                y: cy + tangent_radius * a.sin(),
            });
        }
    }
    coords.push(coords[0].clone());
    Ok(coords)
}

fn serialize_and_validate(pcb: dsn::Pcb) -> Result<String> {
    let file = dsn::DsnFile { pcb };
    let mut bytes = Vec::new();
    {
        let mut writer = ListWriter::new(&mut bytes);
        file.write_dsn(&mut writer)
            .context("serialize Specctra DSN")?;
    }
    let mut text = String::from_utf8(bytes).context("Specctra writer emitted non-UTF-8")?;
    if text.starts_with('\n') {
        text.remove(0);
    }
    text.push('\n');

    let cursor = Cursor::new(text.as_bytes());
    let mut tokenizer = ListTokenizer::new(BufReader::new(cursor));
    dsn::DsnFile::read_dsn(&mut tokenizer)
        .map_err(|error| anyhow::anyhow!("generated DSN failed parser round-trip: {error}"))?;
    Ok(text)
}

#[allow(clippy::too_many_arguments)]
fn build_manifest(
    board_path: &Path,
    source_sha256: &str,
    copper_layers: &[String],
    footprints: &[FootprintModel],
    net_pins: &BTreeMap<String, Vec<String>>,
    net_classes: &BTreeMap<String, String>,
    padstack_names: &BTreeMap<PadstackKey, String>,
    via_names: &BTreeMap<RuleKey, String>,
    locked_routing: &LockedRouting,
) -> Result<String> {
    let components = footprints
        .iter()
        .map(|footprint| ManifestComponent {
            reference: footprint.reference.clone(),
            kiid: footprint.kiid.clone(),
            image_name: footprint.image_name.clone(),
            x_um: footprint.x_um,
            y_um: footprint.y_um,
            rotation_degrees: footprint.rotation_degrees,
            side: "front".to_string(),
            pads: footprint
                .pads
                .iter()
                .map(|pad| ManifestPin {
                    pad_number: pad.number.clone(),
                    dsn_pin: format!("{}-{}", footprint.reference, pad.number),
                    net: pad.net.clone(),
                    padstack_name: padstack_names[&pad.padstack].clone(),
                })
                .collect(),
        })
        .collect();
    let nets = net_pins
        .iter()
        .map(|(name, pins)| ManifestNet {
            name: name.clone(),
            pins: pins.clone(),
            class_name: net_classes[name].clone(),
        })
        .collect();
    let mut padstacks = padstack_names
        .iter()
        .map(|(key, name)| ManifestPadstack {
            name: name.clone(),
            purpose: "pad".to_string(),
            shape: key.shape,
            layers: key.layers.clone(),
            size_x_um: key.size_x_um,
            size_y_um: key.size_y_um,
            drill_um: key.drill_um,
        })
        .collect::<Vec<_>>();
    padstacks.extend(via_names.iter().map(|(rule, name)| ManifestPadstack {
        name: name.clone(),
        purpose: "via".to_string(),
        shape: PadShape::Circle,
        layers: copper_layers.to_vec(),
        size_x_um: rule.via_diameter_um,
        size_y_um: rule.via_diameter_um,
        drill_um: Some(rule.via_drill_um),
    }));
    padstacks.sort_by(|left, right| left.name.cmp(&right.name));

    serde_json::to_string_pretty(&Manifest {
        schema_version: 1,
        board_path: board_path.display().to_string(),
        source_sha256: source_sha256.to_string(),
        coordinate_unit: "um".to_string(),
        resolution: DSN_RESOLUTION as u32,
        supported_profile: SupportedProfile {
            copper_layers: 2,
            component_side: "front".to_string(),
            pad_shapes: ["circle", "rect", "roundrect", "oval"]
                .map(str::to_string)
                .into(),
            existing_routing: !locked_routing.tracks.is_empty() || !locked_routing.vias.is_empty(),
            locked_track_count: locked_routing.tracks.len(),
            locked_via_count: locked_routing.vias.len(),
            copper_zones: false,
            custom_rules: false,
            outline: "one closed loop of straight Edge.Cuts lines".to_string(),
        },
        layers: copper_layers
            .iter()
            .enumerate()
            .map(|(index, layer)| ManifestLayer {
                kicad_name: layer.clone(),
                dsn_name: layer.clone(),
                index,
            })
            .collect(),
        components,
        nets,
        padstacks,
    })
    .context("serialize routing manifest")
}

fn top_level_net_table(tree: &SexpNode) -> BTreeMap<String, String> {
    tree.find_all("net")
        .into_iter()
        .filter_map(|net| {
            let id = net.get(1)?.as_str()?;
            let name = net.get(2)?.as_str()?;
            Some((id.to_string(), name.to_string()))
        })
        .collect()
}

fn resolve_net(net: &SexpNode, table: &BTreeMap<String, String>) -> Option<String> {
    if let Some(name) = net.get(2).and_then(SexpNode::as_str) {
        return (!name.is_empty()).then(|| name.to_string());
    }
    let value = net.get(1)?.as_str()?;
    if let Some(name) = table.get(value).filter(|name| !name.is_empty()) {
        return Some(name.clone());
    }
    // KiCad 10 stores the net name directly as `(net "NAME")`. Older board
    // files used `(net id "NAME")`, handled by the branch above.
    (!value.is_empty()).then(|| value.to_string())
}

fn property_value<'a>(node: &'a SexpNode, property_name: &str) -> Option<&'a str> {
    node.find_all("property")
        .into_iter()
        .find(|property| property.get(1).and_then(SexpNode::as_str) == Some(property_name))?
        .get(2)?
        .as_str()
}

fn point_um(node: &SexpNode, tag: &str) -> Result<(i64, i64)> {
    let point = node.find(tag).with_context(|| format!("missing '{tag}'"))?;
    Ok((
        finite_um(point.get_f64(1), tag)?,
        -finite_um(point.get_f64(2), tag)?,
    ))
}

fn optional_rotation(node: &SexpNode, label: &str) -> Result<f64> {
    if node.get(3).is_none() {
        Ok(0.0)
    } else {
        finite_number(node.get_f64(3), label)
    }
}

fn finite_number(value: Option<f64>, label: &str) -> Result<f64> {
    let value = value.with_context(|| format!("missing {label}"))?;
    if !value.is_finite() {
        bail!("{label} is not finite");
    }
    Ok(value)
}

fn finite_um(value: Option<f64>, label: &str) -> Result<i64> {
    let value = finite_number(value, label)? * UM_PER_MM;
    if value < i64::MIN as f64 || value > i64::MAX as f64 {
        bail!("{label} is outside the supported coordinate range");
    }
    Ok(value.round() as i64)
}

fn roundrect_dimension_nm(value: Option<f64>, label: &str) -> Result<i64> {
    let nanometres = finite_number(value, label)? * 1_000_000.0;
    if nanometres > i64::MAX as f64 {
        bail!("{label} is outside the supported nanometre range");
    }
    Ok(nanometres.round() as i64)
}

fn positive_um(value: Option<f64>, label: &str) -> Result<i64> {
    let value = finite_um(value, label)?;
    if value <= 0 {
        bail!("{label} must be greater than zero");
    }
    Ok(value)
}

fn non_negative_um(value: Option<f64>, label: &str) -> Result<i64> {
    let value = finite_um(value, label)?;
    if value < 0 {
        bail!("{label} must not be negative");
    }
    Ok(value)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use konnect_ipc::IpcRoutingRules;

    fn rules() -> IpcEffectiveRoutingRules {
        ["GND", "VCC"]
            .into_iter()
            .map(|net| {
                (
                    net.to_string(),
                    IpcRoutingRules {
                        class_name: "Default".to_string(),
                        constituents: vec!["Default".to_string()],
                        track_width_mm: Some(0.25),
                        clearance_mm: Some(0.2),
                        via_diameter_mm: Some(0.6),
                        via_drill_mm: Some(0.3),
                    },
                )
            })
            .collect()
    }

    fn native_fixture_rules() -> IpcEffectiveRoutingRules {
        let mut rules = rules();
        for rule in rules.values_mut() {
            rule.track_width_mm = Some(0.2);
        }
        rules
    }

    #[test]
    fn deterministic_export_round_trips_through_specctra_parser() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let first = export_dsn(Path::new("board.kicad_pcb"), source, &rules()).unwrap();
        let second = export_dsn(Path::new("board.kicad_pcb"), source, &rules()).unwrap();

        assert_eq!(first.dsn, second.dsn);
        assert_eq!(first.manifest, second.manifest);
        assert_eq!(first.component_count, 2);
        assert_eq!(first.pad_count, 4);
        assert_eq!(first.net_count, 2);
        assert!(first.dsn.contains("(pcb board.kicad_pcb"));
        assert!(first.dsn.contains("(boundary"));
        assert!(first.dsn.contains("(net GND"));
        assert!(first.dsn.contains("R1-1"));
        let manifest: serde_json::Value = serde_json::from_str(&first.manifest).unwrap();
        assert_eq!(manifest["components"][0]["reference"], "R1");
        assert_eq!(manifest["components"][0]["x_um"], 100_000);
        assert_eq!(manifest["components"][0]["y_um"], -50_000);
        assert_eq!(manifest["components"][0]["rotation_degrees"], 0.0);
        assert_eq!(manifest["components"][0]["side"], "front");
    }

    #[test]
    fn freerouting_owned_dsn_corpus_parses() {
        let source = include_str!("../tests/fixtures/freerouting_issue269_minimal_v2_3_0.dsn");
        let mut tokenizer = ListTokenizer::new(BufReader::new(Cursor::new(source.as_bytes())));
        dsn::DsnFile::read_dsn(&mut tokenizer)
            .expect("Freerouting v2.3.0 corpus fixture must remain parseable");
    }

    #[test]
    fn native_kicad_dsn_rewrites_manifest_identifiers_without_changing_semantics() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let native = include_str!("../tests/fixtures/specctra_two_resistors.native-kicad-10.dsn");
        let baseline = export_dsn(
            Path::new("board.kicad_pcb"),
            source,
            &native_fixture_rules(),
        )
        .unwrap();
        let adopted = adopt_native_dsn(baseline, native.to_string()).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();

        assert_eq!(adopted.dsn, native);
        assert_eq!(
            manifest["components"][0]["image_name"],
            "Resistor_SMD:R_0402"
        );
        assert_eq!(
            manifest["components"][0]["pads"][0]["padstack_name"],
            "Rect[T]Pad_600.000000x500.000000_um"
        );
        assert_eq!(manifest["nets"][0]["class_name"], "kicad_default");
        assert!(manifest["padstacks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|padstack| padstack["name"] == "Via[0-1]_600:300_um"));
    }

    /// Optional local parity check against the Freerouting engine. CI does not
    /// install Java or Freerouting; maintainers can opt in with
    /// `FREEROUTING_JAR=/path/to/freerouting.jar cargo test -p konnect-core
    /// freerouting_accepts_exported_fixture -- --ignored`.
    #[test]
    #[ignore = "requires Java and FREEROUTING_JAR"]
    fn freerouting_accepts_exported_fixture() {
        let jar = std::env::var_os("FREEROUTING_JAR").expect("set FREEROUTING_JAR");
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let temp = tempfile::tempdir().expect("tempdir");
        let board_path = temp.path().join("board.kicad_pcb");
        let dsn_path = temp.path().join("board.dsn");
        let ses_path = temp.path().join("board.ses");
        std::fs::write(&board_path, source).expect("write board fixture");
        let export = export_dsn(&board_path, source, &rules()).unwrap();
        std::fs::write(&dsn_path, export.dsn).expect("write DSN");

        let output = std::process::Command::new("java")
            .arg("-jar")
            .arg(jar)
            .arg("-de")
            .arg(&dsn_path)
            .arg("-do")
            .arg(&ses_path)
            .arg("-mp")
            .arg("2")
            .output()
            .expect("launch Freerouting");

        assert!(
            output.status.success(),
            "Freerouting refused generated DSN:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            std::fs::metadata(&ses_path)
                .map(|metadata| metadata.len() > 0)
                .unwrap_or(false),
            "Freerouting did not produce a non-empty SES"
        );
        let ses = std::fs::read_to_string(&ses_path).expect("read Freerouting SES");
        crate::specctra_ses::parse_import_plan(&board_path, source, &export.manifest, &ses)
            .expect("Freerouting SES must pass Konnect's strict import planner");
    }

    /// Real-engine parity for KiCad 10's native exporter identifiers plus the
    /// rewritten revision manifest consumed by the strict SES planner.
    #[test]
    #[ignore = "requires Java and FREEROUTING_JAR"]
    fn freerouting_round_trips_native_kicad_export() {
        let jar = std::env::var_os("FREEROUTING_JAR").expect("set FREEROUTING_JAR");
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let native = include_str!("../tests/fixtures/specctra_two_resistors.native-kicad-10.dsn");
        let temp = tempfile::tempdir().expect("tempdir");
        let board_path = temp.path().join("board.kicad_pcb");
        let dsn_path = temp.path().join("board.dsn");
        let ses_path = temp.path().join("board.ses");
        std::fs::write(&board_path, source).expect("write board fixture");
        let baseline = export_dsn(&board_path, source, &native_fixture_rules()).unwrap();
        let export = adopt_native_dsn(baseline, native.to_string()).unwrap();
        std::fs::write(&dsn_path, &export.dsn).expect("write native DSN");

        let output = std::process::Command::new("java")
            .arg("-jar")
            .arg(jar)
            .arg("-de")
            .arg(&dsn_path)
            .arg("-do")
            .arg(&ses_path)
            .arg("-mp")
            .arg("2")
            .output()
            .expect("run Freerouting");
        assert!(
            output.status.success(),
            "Freerouting failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let ses = std::fs::read_to_string(&ses_path).expect("read SES");
        let plan =
            crate::specctra_ses::parse_import_plan(&board_path, source, &export.manifest, &ses)
                .expect("strict SES planner accepts native KiCad identifiers");
        assert!(!plan.tracks.is_empty() || !plan.vias.is_empty());
    }

    const ROTATED_SOIC: &str = include_str!("../tests/fixtures/specctra_soic8_roundrect.kicad_pcb");
    const ROTATED_SOIC_NATIVE: &str =
        include_str!("../tests/fixtures/specctra_soic8_roundrect.native-kicad-10.dsn");

    fn stock_soic_rules() -> IpcEffectiveRoutingRules {
        let rule = native_fixture_rules()["GND"].clone();
        (1..=8)
            .map(|pin| (format!("SIG{pin}"), rule.clone()))
            .collect()
    }

    #[test]
    fn rotated_stock_soic_exports_roundrect_without_double_rotation() {
        let exported = export_dsn(
            Path::new("soic8.kicad_pcb"),
            ROTATED_SOIC,
            &stock_soic_rules(),
        )
        .unwrap();
        let tree = parse_dsn_sexp(&exported.dsn).unwrap();
        let library = tree.find("library").unwrap();
        let image = library
            .find_all("image")
            .into_iter()
            .find(|image| image.get(1).and_then(SexpNode::as_str) == Some("konnect_image_U1"))
            .unwrap();
        assert_eq!(image.find_all("pin").len(), 8);
        assert!(image
            .find_all("pin")
            .iter()
            .all(|pin| pin.find("rotate").is_none()));
        let shape = library.find("padstack").unwrap().find("shape").unwrap();
        assert_eq!(shape.get(1).unwrap().head(), Some("polygon"));
        adopt_native_dsn(exported, ROTATED_SOIC_NATIVE.to_string()).unwrap();
    }

    #[test]
    fn native_only_export_authenticates_identity_without_a_reverse_manifest() {
        let path = Path::new("soic8.kicad_pcb");
        let exported = native_export_only(
            path,
            ROTATED_SOIC,
            ROTATED_SOIC_NATIVE.into(),
            "custom rules",
        )
        .unwrap();
        assert!(!exported.ses_importable);
        assert_eq!(exported.dsn, ROTATED_SOIC_NATIVE);
        assert_eq!(exported.source_sha256, sha256_hex(ROTATED_SOIC.as_bytes()));
        let error =
            crate::specctra_ses::parse_import_plan(path, ROTATED_SOIC, &exported.manifest, "")
                .unwrap_err();
        assert!(error.to_string().contains("native-only"));
        let changed = ROTATED_SOIC_NATIVE.replace("(pins U2-1 U1-1)", "(pins U2-2 U1-1)");
        assert!(
            native_export_only(path, ROTATED_SOIC, changed, "custom rules")
                .unwrap_err()
                .to_string()
                .contains("net or pin membership")
        );
    }

    #[test]
    fn native_only_export_binds_back_pads_and_ignores_mechanical_keepout_identity() {
        let source = include_str!("../tests/fixtures/specctra_stock_soic8_bottom_4layer.kicad_pcb");
        let native = include_str!(
            "../tests/fixtures/specctra_stock_soic8_bottom_4layer.native-kicad-10.dsn"
        );
        let path = Path::new("bottom.kicad_pcb");
        let exported = native_export_only(path, source, native.into(), "custom rules").unwrap();
        assert_eq!(
            (
                exported.component_count,
                exported.pad_count,
                exported.net_count
            ),
            (2, 16, 8)
        );
        assert!(!exported.ses_importable);
        let changed = native.replace("345) 1 -2475 1905", "346) 1 -2475 1905");
        assert!(native_export_only(path, source, changed, "custom rules").is_err());
    }

    #[test]
    fn native_only_export_tolerates_serialized_relative_rotation_arithmetic() {
        let source = ROTATED_SOIC
            .replace("(at -2.475 -1.905 90)", "(at -2.475 -1.905 90.1)")
            .replace("(at -2.475 -1.905)", "(at -2.475 -1.905 0.1)");
        let native = ROTATED_SOIC_NATIVE.replace("0 1 -2475 1905)", "0 (rotate 0.1) 1 -2475 1905)");
        assert_ne!((90.1_f64 - 90.0).to_bits(), 0.1_f64.to_bits());
        native_export_only(
            Path::new("soic8.kicad_pcb"),
            &source,
            native,
            "custom rules",
        )
        .unwrap();
    }

    #[test]
    fn roundrect_outline_does_not_shrink_fractional_pad_dimensions() {
        let pads = COVERAGE_ROUTING_PAD
            .replace("smd rect", "smd roundrect")
            .replace(
                "(size 2 2)",
                "(size 1.50049 0.80049) (roundrect_rratio 0.25)",
            );
        let board = coverage_routing_board(&pads, "", "");
        let exported = export_dsn(Path::new("board.kicad_pcb"), &board, &rules()).unwrap();
        let identity = dsn_identity(&parse_dsn_sexp(&exported.dsn).unwrap()).unwrap();
        let pin = &identity.components["U1"].pins["1"];
        let DsnGeometry::Polygon { points, .. } = &identity.padstacks[&pin.padstack][0].geometry
        else {
            panic!("expected a roundrect polygon");
        };
        for (actual, expected) in [
            (
                points.iter().map(|(x, _)| x.abs()).fold(0.0, f64::max),
                750.245,
            ),
            (
                points.iter().map(|(_, y)| y.abs()).fold(0.0, f64::max),
                400.245,
            ),
        ] {
            assert!(
                actual >= expected - 1e-8,
                "fractional copper shrank from {expected} to {actual}"
            );
            assert!(actual <= expected + 0.501);
        }
    }

    #[test]
    fn native_receipt_refuses_mismatched_placements_pins_and_layers() {
        for (from, to, expected) in [
            ("(layer B.Cu", "(layer In1.Cu", "copper-layer inventory"),
            (
                "(boundary",
                "(layer In1.Cu (type signal) (property (index 2))) (boundary",
                "copper-layer inventory",
            ),
            ("U2 108000 -56000 front 0", "U2 108000 -56000 back 0", "placement cannot be bound"),
            ("U2 108000 -56000 front 0", "U2 108001 -56000 front 0", "placement cannot be bound"),
            ("U2 108000 -56000 front 0", "U2 108000 -56001 front 0", "placement cannot be bound"),
            ("U2 108000 -56000 front 0", "U2 108000 -56000 front 1", "placement cannot be bound"),
            ("1 -2475 1905)", "1 -2476 1905)", "cannot bind pin"),
            ("1 -2475 1905)", "1 -2475 1906)", "cannot bind pin"),
            (
                "0 1 -2475 1905)",
                "0 1 -2475 1905) (pin RoundRect[T]Pad_1950.000000x600.000000_150.571000_um_0.000000_0 9 3000 1905)",
                "changed pins for component",
            ),
            (
                "U2 108000 -56000 front 0 (PN SOIC8_fixture))",
                "U2 108000 -56000 front 0 (PN SOIC8_fixture)) (place U3 113000 -56000 front 0)",
                "changed the component inventory",
            ),
            (
                "(pin RoundRect[T]Pad_1950.000000x600.000000_150.571000_um_0.000000_0 1",
                "(pin MissingPadstack 1",
                "cannot bind pin",
            ),
        ] {
            let changed = ROTATED_SOIC_NATIVE.replace(from, to);
            assert_ne!(changed, ROTATED_SOIC_NATIVE, "ineffective DSN mutation {from}");
            let error = native_export_only(
                Path::new("board.kicad_pcb"), ROTATED_SOIC, changed, "custom rules"
            ).unwrap_err();
            assert!(error.to_string().contains(expected), "{to}: {error:#}");
        }
    }

    #[test]
    fn native_receipt_refuses_board_identity_gaps() {
        for (from, to, expected) in [
            ("(kicad_pcb", "(other_pcb", "snapshot root"),
            (
                "(property \"Reference\" \"U2\"",
                "(property \"Reference\" \"U1\"",
                "repeats footprint",
            ),
            (
                "(pad \"2\" smd roundrect",
                "(pad \"1\" smd roundrect",
                "repeats pin",
            ),
            ("(net \"SIG1\")", "", "changed net or pin membership"),
        ] {
            let changed = ROTATED_SOIC.replacen(from, to, 1);
            assert_ne!(changed, ROTATED_SOIC, "ineffective board mutation {from}");
            let error = native_export_only(
                Path::new("board.kicad_pcb"),
                &changed,
                ROTATED_SOIC_NATIVE.into(),
                "custom rules",
            )
            .unwrap_err();
            assert!(error.to_string().contains(expected), "{to}: {error:#}");
        }
    }

    #[test]
    fn native_pad_shape_parser_preserves_aperture_coordinates() {
        for (source, expected) in [
            (
                "(circle F.Cu 600 15 -25)",
                DsnGeometry::Circle {
                    diameter: 600.0,
                    offset: (15.0, -25.0),
                },
            ),
            (
                "(path F.Cu 600 -375 0 375 0)",
                DsnGeometry::Path {
                    width: 600.0,
                    points: vec![(-375.0, 0.0), (375.0, 0.0)],
                },
            ),
        ] {
            let geometry = dsn_shape(&parse_sexp(source).unwrap()).unwrap();
            assert_eq!(geometry.layer, "F.Cu");
            assert_eq!(geometry.geometry, expected);
        }
    }

    #[test]
    fn native_pad_shape_parser_refuses_malformed_apertures() {
        for (source, expected) in [
            ("(circle F.Cu 0)", "diameter must be positive"),
            (
                "(circle F.Cu 600 0)",
                "malformed or unsupported shape 'circle'",
            ),
            ("(circle F.Cu NaN)", "not finite"),
            (
                "(rect F.Cu -10 -10 10)",
                "malformed or unsupported shape 'rect'",
            ),
            (
                "(path F.Cu 100 0 0)",
                "malformed or unsupported shape 'path'",
            ),
            (
                "(path F.Cu 0 -10 0 10 0)",
                "malformed or unsupported shape 'path'",
            ),
            (
                "(path F.Cu 100 -10 0 10 0 10)",
                "incomplete coordinate pair",
            ),
            (
                "(polygon F.Cu 0 -10 0 10 0)",
                "malformed or unsupported shape 'polygon'",
            ),
            (
                "(polygon F.Cu -1 -10 0 10 0 0 10)",
                "malformed or unsupported shape 'polygon'",
            ),
        ] {
            let error = dsn_shape(&parse_sexp(source).unwrap()).unwrap_err();
            assert!(error.to_string().contains(expected), "{source}: {error:#}");
        }
    }

    #[test]
    fn native_courtyard_projection_rejects_truncated_polygons() {
        for outline in [
            "(outline (polygon signal 0 0 0 10 0))",
            "(outline (polygon signal 0 0 0 10 0 10 10 0))",
        ] {
            let dsn = format!("(pcb board (library (image part {outline})))");
            let error = checked_native_tree(&dsn).unwrap_err();
            assert!(
                error.to_string().contains("malformed polygon outline"),
                "{outline}: {error:#}"
            );
        }
    }

    #[test]
    fn roundrect_radius_above_half_the_minor_axis_is_refused() {
        let pad = COVERAGE_ROUTING_PAD
            .replace("smd rect", "smd roundrect")
            .replace("(size 2 2)", "(size 2 2) (roundrect_rratio 0.501)");
        let board = coverage_routing_board(&pad, "", "");
        let error = export_dsn(Path::new("board.kicad_pcb"), &board, &rules()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("pad U1-1 roundrect radius ratio is outside"),
            "{error:#}"
        );
    }

    #[test]
    fn native_pin_half_micrometre_rounding_is_adoptable() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let native = include_str!("../tests/fixtures/specctra_two_resistors.native-kicad-10.dsn");
        let baseline = export_dsn(
            Path::new("board.kicad_pcb"),
            source,
            &native_fixture_rules(),
        )
        .unwrap();
        adopt_native_dsn(baseline, native.replace("1 -500 0)", "1 -500.5 0)")).unwrap();
    }

    #[test]
    fn nonzero_relative_pad_rotation_is_normalized() {
        let board = coverage_routing_board(
            &COVERAGE_ROUTING_PAD.replace("(at 1 2 90)", "(at 1 2 75)"),
            "",
            "",
        );
        let export = export_dsn(Path::new("board.kicad_pcb"), &board, &rules()).unwrap();
        let identity = dsn_identity(&parse_dsn_sexp(&export.dsn).unwrap()).unwrap();
        assert_eq!(identity.components["U1"].rotation, 90.0);
        assert_eq!(identity.components["U1"].pins["1"].rotation, 345.0);
    }

    #[test]
    fn oval_uses_its_minor_axis_as_path_width() {
        let board = coverage_routing_board(
            &COVERAGE_ROUTING_PAD
                .replace("smd rect", "smd oval")
                .replace("(size 2 2)", "(size 3 2)"),
            "",
            "",
        );
        let exported = export_dsn(Path::new("board.kicad_pcb"), &board, &rules()).unwrap();
        let tree = parse_dsn_sexp(&exported.dsn).unwrap();
        let shape = tree
            .find("library")
            .unwrap()
            .find("padstack")
            .unwrap()
            .find("shape")
            .unwrap();
        let path = shape.get(1).unwrap();
        assert_eq!(path.head(), Some("path"));
        assert_eq!(path.get_f64(2), Some(2000.0));
        assert_eq!(
            (path.get_f64(3), path.get_f64(5)),
            (Some(-500.0), Some(500.0))
        );
    }

    #[test]
    fn differing_copper_padstack_is_refused_with_pad_identity() {
        let pad = COVERAGE_ROUTING_PAD.replace(
            "(size 2 2)",
            r#"(size 2 2) (padstack (mode custom) (layer "B.Cu" (shape rect) (size 3 2)))"#,
        );
        let board = coverage_routing_board(&pad, "", "");
        let error = export_dsn(Path::new("board.kicad_pcb"), &board, &rules()).unwrap_err();
        assert!(error.to_string().contains("pad U1-1"));
        assert!(error.to_string().contains("padstack"));
    }

    #[test]
    fn incomplete_effective_rules_are_refused() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let mut rules = rules();
        rules.get_mut("GND").unwrap().via_drill_mm = None;
        let error = export_dsn(Path::new("board.kicad_pcb"), source, &rules)
            .unwrap_err()
            .to_string();
        assert!(error.contains("via drill"), "{error}");
    }

    #[test]
    fn unlocked_routing_is_refused_before_export() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb").replace(
            "\n)",
            "\n  (segment (start 1 1) (end 2 2) (width 0.2) (layer \"F.Cu\") (net 1))\n)",
        );
        let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains("existing track segment"), "{error}");
    }

    #[test]
    fn locked_tracks_and_vias_are_exported_as_fixed_wiring() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors_locked.kicad_pcb");
        let export = export_dsn(Path::new("board.kicad_pcb"), source, &rules()).unwrap();
        assert!(export.dsn.contains("(type fix)"));
        assert!(export.dsn.contains("(path F.Cu 250"));
        assert!(export.dsn.contains("(via konnect_via_"));
        let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
        assert_eq!(manifest["supported_profile"]["existing_routing"], true);
        assert_eq!(manifest["supported_profile"]["locked_track_count"], 1);
        assert_eq!(manifest["supported_profile"]["locked_via_count"], 1);
    }

    #[test]
    fn freerouting_session_preserves_locked_routing_outside_the_import_plan() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors_locked.kicad_pcb");
        let native =
            include_str!("../tests/fixtures/specctra_two_resistors_locked.native-kicad-10.dsn");
        let ses =
            include_str!("../tests/fixtures/specctra_two_resistors_locked.freerouting-2.3.0.ses");
        let temp = tempfile::tempdir().unwrap();
        let board_path = temp.path().join("specctra_two_resistors_locked.kicad_pcb");
        std::fs::write(&board_path, source).unwrap();
        let baseline = export_dsn(&board_path, source, &native_fixture_rules()).unwrap();
        let adopted = adopt_native_dsn(baseline, native.to_string()).unwrap();
        let plan =
            crate::specctra_ses::parse_import_plan(&board_path, source, &adopted.manifest, ses)
                .unwrap();
        assert_eq!(plan.locked_track_count, 1);
        assert_eq!(plan.locked_via_count, 1);
        assert!(!plan.tracks.is_empty() || !plan.vias.is_empty());
    }

    #[test]
    fn locked_arcs_are_refused_instead_of_approximated() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors_locked_arc.kicad_pcb");
        let error = export_dsn(Path::new("board.kicad_pcb"), source, &rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains("locked routed arc"), "{error}");
        assert!(error.contains("without approximation"), "{error}");
    }

    #[test]
    fn branched_outline_is_refused() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb")
            .replace(
                "\n)",
                "\n  (gr_line (start 80 30) (end 90 40) (stroke (width 0.05) (type default)) (layer \"Edge.Cuts\") (uuid \"branch\"))\n)",
            );
        let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains("degree"), "{error}");
    }

    #[test]
    fn nonzero_pad_shape_offset_is_refused() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb").replacen(
            "(size 0.6 0.5)",
            "(size 0.6 0.5)\n\t\t\t(offset 0.1 0)",
            1,
        );
        let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains("shape offset"), "{error}");
    }

    fn coverage_routing_board(pad: &str, footprint_extra: &str, board_extra: &str) -> String {
        format!(
            r#"(kicad_pcb
          (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
          (footprint "Fixture" (layer "F.Cu") (uuid "fixture") (at 10 20 90)
            (property "Reference" "U1") {pad} {footprint_extra})
          (gr_line (start 0 0) (end 30 0) (layer "Edge.Cuts"))
          (gr_line (start 30 0) (end 30 30) (layer "Edge.Cuts"))
          (gr_line (start 30 30) (end 0 30) (layer "Edge.Cuts"))
          (gr_line (start 0 30) (end 0 0) (layer "Edge.Cuts"))
          {board_extra})"#
        )
    }

    const COVERAGE_ROUTING_PAD: &str = r#"(pad "1" smd rect (at 1 2 90)
        (size 2 2) (layers "F.Cu" "F.Mask") (net "GND"))"#;

    #[test]
    fn export_refuses_unsupported_pad_geometry_without_writing_the_board() {
        let cases = [
            ("smd rect", "np_thru_hole rect", "has type"),
            ("smd rect", "smd custom", "has shape"),
            ("(size 2 2)", "(size 0 2)", "pad width"),
            ("(size 2 2)", "(size 2 -1)", "pad height"),
            ("smd rect", "smd circle", ""),
            ("(size 2 2)", "(size NaN 2)", "pad width"),
            ("(at 1 2 90)", "(at 1e30 2 90)", "pad x"),
            ("(at 1 2 90)", "(at 1 NaN 90)", "pad y"),
            (
                "(layers \"F.Cu\" \"F.Mask\")",
                "(layers \"B.Cu\")",
                "copper layers",
            ),
            (
                "(size 2 2)",
                "(size 2 2) (clearance 0.1)",
                "clearance override",
            ),
            (
                "(size 2 2)",
                "(size 2 2) (remove_unused_layers yes)",
                "removes copper",
            ),
            ("(size 2 2)", "(size 2 2) (offset 0 0.1)", "shape offset"),
            ("(size 2 2)", "(size 2 2) (offset 0.1 0)", "shape offset"),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.kicad_pcb");
        for (from, to, expected) in cases {
            let pad = COVERAGE_ROUTING_PAD.replace(from, to);
            assert_ne!(
                pad, COVERAGE_ROUTING_PAD,
                "ineffective fixture mutation {from}"
            );
            let board = coverage_routing_board(&pad, "", "");
            std::fs::write(&path, &board).unwrap();
            if expected.is_empty() {
                let export = export_dsn(&path, &board, &rules()).unwrap();
                assert_eq!(export.pad_count, 1);
            } else {
                let error = format!("{:#}", export_dsn(&path, &board, &rules()).unwrap_err());
                assert!(error.contains(expected), "{from} -> {to}: {error}");
            }
            assert_eq!(std::fs::read_to_string(&path).unwrap(), board);
        }
    }

    #[test]
    fn through_hole_export_preserves_both_copper_layers_and_refuses_bad_drills() {
        let pad = COVERAGE_ROUTING_PAD
            .replace("smd rect", "thru_hole circle")
            .replace("(layers \"F.Cu\" \"F.Mask\")", "(layers \"*.Cu\")")
            .replace("(size 2 2)", "(size 2 2) (drill 0.8)");
        let source = coverage_routing_board(&pad, "", "");
        let export = export_dsn(Path::new("board.kicad_pcb"), &source, &rules()).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
        let through = manifest["padstacks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["purpose"] == "pad")
            .unwrap();
        assert_eq!(through["layers"], serde_json::json!(["F.Cu", "B.Cu"]));
        assert_eq!(through["drill_um"], 800);
        for (from, to, expected) in [
            ("(drill 0.8)", "(drill oval 0.8 1)", "oval drill"),
            ("(drill 0.8)", "(drill 2)", "not smaller"),
            ("(drill 0.8)", "(drill -1)", "pad drill"),
            ("(layers \"*.Cu\")", "(layers \"F.Cu\")", "copper layers"),
            ("(size 2 2)", "(size 2 3)", "equal X/Y size"),
        ] {
            let changed = source.replace(from, to);
            assert_ne!(source, changed);
            let error = format!(
                "{:#}",
                export_dsn(Path::new("board.kicad_pcb"), &changed, &rules()).unwrap_err()
            );
            assert!(error.contains(expected), "{to}: {error}");
        }
        let explicit = source.replace("(layers \"*.Cu\")", "(layers \"F.Cu\" \"B.Cu\")");
        assert!(export_dsn(Path::new("board.kicad_pcb"), &explicit, &rules()).is_ok());
    }

    #[test]
    fn routing_export_refuses_board_features_it_cannot_represent() {
        let source = coverage_routing_board(COVERAGE_ROUTING_PAD, "", "");
        for (from, to, expected) in [
            ("kicad_pcb", "schematic", "root is not"),
            (
                "(2 \"B.Cu\" signal)",
                "(2 \"In1.Cu\" signal)",
                "exactly F.Cu and B.Cu",
            ),
            ("(layer \"F.Cu\")", "(layer \"B.Cu\")", "footprint on"),
            (
                "(uuid \"fixture\")",
                "(uuid \"fixture\") (clearance 0.1)",
                "clearance override",
            ),
            (
                "(property \"Reference\" \"U1\")",
                "(property \"Reference\" \"\")",
                "Reference property",
            ),
            ("(net \"GND\")", "", "connected pad"),
            (
                "(start 0 0) (end 30 0)",
                "(start 0 0) (end 0 0)",
                "zero-length Edge.Cuts",
            ),
            (
                "(gr_line (start 0 0)",
                "(gr_arc (start 0 0)",
                "not a straight line",
            ),
        ] {
            let changed = source.replacen(from, to, 1);
            assert_ne!(source, changed);
            let error = format!(
                "{:#}",
                export_dsn(Path::new("board.kicad_pcb"), &changed, &rules()).unwrap_err()
            );
            assert!(error.contains(expected), "{from} -> {to}: {error}");
        }
        for (extra, expected) in [
            ("(zone (net 1))", "zone or rule area"),
            ("(arc (start 0 0))", "existing routed arc"),
            ("(via (at 5 5))", "via that is not locked"),
            ("(segment (locked yes) (start 1 1) (end 1 1) (width 0.2) (layer F.Cu) (net GND))", "zero length"),
            ("(segment (locked yes) (start 1 1) (end 2 2) (width 0.2) (layer In1.Cu) (net GND))", "unsupported layer"),
            ("(via (locked yes) (type blind) (at 5 5))", "only through vias"),
            ("(via (locked yes) (at 5 5) (layers F.Cu) (net GND))", "layer span"),
            ("(via (locked yes) (at 5 5) (layers F.Cu B.Cu) (net GND) (size 1) (drill oval 0.3 0.4))", "oval drill"),
            ("(via (locked yes) (at 5 5) (layers F.Cu B.Cu) (net GND) (size 1) (drill 1))", "not smaller"),
        ] {
            let board = coverage_routing_board(COVERAGE_ROUTING_PAD, "", extra);
            let error = format!("{:#}", export_dsn(Path::new("board.kicad_pcb"), &board, &rules()).unwrap_err());
            assert!(error.contains(expected), "{extra}: {error}");
        }
    }

    #[test]
    fn effective_rule_validation_refuses_missing_nonfinite_and_impossible_dimensions() {
        let source = coverage_routing_board(COVERAGE_ROUTING_PAD, "", "");
        for (field, value, expected) in [
            ("track", None, "track width"),
            ("track", Some(0.0), "track width"),
            ("track", Some(f64::NAN), "track width"),
            ("clearance", None, "clearance"),
            ("clearance", Some(-0.1), "clearance"),
            ("diameter", Some(0.1), "not smaller"),
            ("drill", Some(0.0), "via drill"),
        ] {
            let mut settings = rules();
            let net = settings.get_mut("GND").unwrap();
            match field {
                "track" => net.track_width_mm = value,
                "clearance" => net.clearance_mm = value,
                "diameter" => net.via_diameter_mm = value,
                "drill" => net.via_drill_mm = value,
                _ => unreachable!(),
            }
            let error = format!(
                "{:#}",
                export_dsn(Path::new("board.kicad_pcb"), &source, &settings).unwrap_err()
            );
            assert!(error.contains(expected), "{field}: {error}");
        }
        let mut settings = rules();
        settings.remove("GND");
        let error = export_dsn(Path::new("board.kicad_pcb"), &source, &settings).unwrap_err();
        assert!(error.to_string().contains("no effective routing rules"));
        let mut zero_clearance = rules();
        zero_clearance.get_mut("GND").unwrap().clearance_mm = Some(0.0);
        assert!(export_dsn(Path::new("board.kicad_pcb"), &source, &zero_clearance).is_ok());
    }

    #[test]
    fn native_dsn_adoption_refuses_changed_geometry_identity_and_routing_rules() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let native = include_str!("../tests/fixtures/specctra_two_resistors.native-kicad-10.dsn");
        for (from, to, expected) in [
            ("(unit um)", "(unit mm)", "micrometre coordinate system"),
            (
                "(layer B.Cu",
                "(layer In1.Cu",
                "renamed a KiCad copper layer",
            ),
            ("(index 1)", "(index 2)", "omitted layer index"),
            ("(index 1)", "(index 0)", "repeats a layer index"),
            (
                "(place R1 100000 -50000 front 0",
                "(place R1 100001 -50000 front 0",
                "changed placement or pins",
            ),
            (
                "(place R1 100000 -50000 front 0",
                "(place R1 100000 -50001 front 0",
                "changed placement or pins",
            ),
            (
                "(place R1 100000 -50000 front 0",
                "(place R1 100000 -50000 back 0",
                "changed placement or pins",
            ),
            (
                "(place R1 100000 -50000 front 0",
                "(place R1 100000 -50000 front 90",
                "changed placement or pins",
            ),
            (
                "(place R1 100000 -50000 front 0",
                "(place R3 100000 -50000 front 0",
                "omitted component",
            ),
            ("1 -500 0)", "1 -501 0)", "changed pin position"),
            ("2 500 0)", "3 500 0)", "changed placement or pins"),
            (
                "1 -500 0)",
                "1 -500 0)\n      (pin Rect[T]Pad_600.000000x500.000000_um 1 -500 0)",
                "repeats a pin number",
            ),
            (
                "(rect F.Cu -300 -250 300 250)",
                "(rect F.Cu -301 -250 300 250)",
                "changed pad geometry",
            ),
            (
                "(circle F.Cu 600)",
                "(circle F.Cu 601)",
                "changed via geometry",
            ),
            (
                "(pins R1-1 R2-2)",
                "(pins R1-2 R2-2)",
                "changed net or pin membership",
            ),
            (
                "(class kicad_default GND VCC",
                "(class kicad_default GND",
                "no unique class",
            ),
            (
                "(class kicad_default GND VCC",
                "(class kicad_default GND UNKNOWN",
                "invalid or duplicate class",
            ),
            ("(width 200)", "(width 201)", "changed width or clearance"),
            (
                "(clearance 200)",
                "(clearance 201)",
                "changed width or clearance",
            ),
        ] {
            let changed = native.replace(from, to);
            assert_ne!(changed, native, "ineffective fixture mutation {from}");
            let baseline = export_dsn(
                Path::new("board.kicad_pcb"),
                source,
                &native_fixture_rules(),
            )
            .unwrap();
            let error = format!("{:#}", adopt_native_dsn(baseline, changed).unwrap_err());
            assert!(error.contains(expected), "{from} -> {to}: {error}");
        }
    }

    #[test]
    fn export_rejects_duplicate_references_pads_empty_boards_and_disconnected_outlines() {
        let pad = COVERAGE_ROUTING_PAD;
        for (source, expected) in [
            (coverage_routing_board("", "", ""), "has no pads"),
            (coverage_routing_board(pad, pad, ""), "duplicate pad number"),
            (
                coverage_routing_board(
                    pad,
                    "",
                    &format!(
                        r#"(footprint "Second" (layer "F.Cu") (uuid "second") (at 1 1) (property "Reference" "U1") {pad})"#
                    ),
                ),
                "duplicate footprint reference",
            ),
            (
                coverage_routing_board(pad, "", "")
                    .replace("(footprint \"Fixture\"", "(unused \"Fixture\""),
                "at least one footprint",
            ),
            (
                coverage_routing_board(
                    pad,
                    "",
                    r#"(gr_line (start 40 0) (end 50 0) (layer "Edge.Cuts")) (gr_line (start 50 0) (end 50 10) (layer "Edge.Cuts")) (gr_line (start 50 10) (end 40 10) (layer "Edge.Cuts")) (gr_line (start 40 10) (end 40 0) (layer "Edge.Cuts"))"#,
                ),
                "not one closed loop",
            ),
        ] {
            let error = format!(
                "{:#}",
                export_dsn(Path::new("board.kicad_pcb"), &source, &rules()).unwrap_err()
            );
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn native_adoption_refuses_duplicate_and_empty_identity_collections() {
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let native = include_str!("../tests/fixtures/specctra_two_resistors.native-kicad-10.dsn");
        let place = "(place R1 100000 -50000 front 0 (PN 10k))";
        let net = "(net GND\n      (pins R1-2 R2-1)\n    )";
        let class = "(class kicad_default GND VCC\n      (circuit\n        (use_via \"Via[0-1]_600:300_um\")\n      )\n      (rule\n        (width 200)\n        (clearance 200)\n      )\n    )";
        let layer =
            "(layer B.Cu\n      (type signal)\n      (property\n        (index 1)\n      )\n    )";
        for (from, to, expected) in [
            ("(image Resistor_SMD:R_0402\n", "(image Resistor_SMD:R_0402)\n    (image Resistor_SMD:R_0402\n".to_string(), "repeats image"),
            ("(shape (rect F.Cu -300 -250 300 250))", "".into(), "empty or duplicate padstack"),
            ("(padstack Rect[T]Pad_600.000000x500.000000_um\n", "(padstack Rect[T]Pad_600.000000x500.000000_um (shape (rect F.Cu -300 -250 300 250)) (attach off))\n    (padstack Rect[T]Pad_600.000000x500.000000_um\n".into(), "empty or duplicate padstack"),
            (place, format!("{place} {place}"), "repeats component"),
            ("(pins R1-2 R2-1)", "(pins)".into(), "empty or duplicate net"),
            (net, format!("{net} {net}"), "empty or duplicate net"),
            ("(class kicad_default GND VCC", "(class kicad_default".into(), "invalid or duplicate class"),
            (class, format!("{class} {class}"), "invalid or duplicate class"),
            (layer, "".into(), "changed the copper-layer count"),
            (class, "".into(), "changed the routing-class count"),
            (place, "".into(), "changed the component count"),
            ("1 -500 0)", "1 -500.501 0)".into(), "changed pin position"),
            ("1 -500 0)", "1 -1e30 0)".into(), "changed pin position"),
            ("1 -500 0)", "1 1e30 0)".into(), "changed pin position"),
        ] {
            let changed = native.replace(from,&to);
            assert_ne!(changed,native,"ineffective pinned fixture mutation: {from}");
            let baseline = export_dsn(Path::new("board.kicad_pcb"),source,&native_fixture_rules()).unwrap();
            let error = format!("{:#}",adopt_native_dsn(baseline,changed).unwrap_err());
            assert!(error.contains(expected),"{from} -> {to}: {error}");
        }
    }

    #[test]
    fn export_resolves_legacy_aliases_and_refuses_routing_outside_effective_rules() {
        for pad in [
            COVERAGE_ROUTING_PAD.replace("(net \"GND\")", "(net 7)"),
            COVERAGE_ROUTING_PAD.replace("(net \"GND\")", "(net 7 \"GND\")"),
        ] {
            let source = coverage_routing_board(&pad, "", r#"(net 7 "GND")"#);
            let exported = export_dsn(Path::new("board.kicad_pcb"), &source, &rules()).unwrap();
            let manifest: serde_json::Value = serde_json::from_str(&exported.manifest).unwrap();
            assert_eq!(manifest["nets"][0]["name"], "GND");
            assert_eq!(manifest["components"][0]["pads"][0]["net"], "GND");
        }
        let zero_offset = COVERAGE_ROUTING_PAD.replace("(size 2 2)", "(size 2 2) (offset 0 0)");
        assert!(export_dsn(
            Path::new("board.kicad_pcb"),
            &coverage_routing_board(&zero_offset, "", ""),
            &rules()
        )
        .is_ok());
        for (routing,expected) in [
            ("(segment (locked yes) (start 1 1) (end 2 2) (width 0.25) (layer F.Cu) (net OTHER))","unknown routing net"),
            ("(via (locked yes) (at 5 5) (layers F.Cu B.Cu) (net GND) (size 0.8) (drill 0.3))","effective via rule"),
            ("(via (locked yes) (at 5 5) (layers F.Cu B.Cu) (net GND) (size 0.6) (drill 0.2))","effective via rule"),
        ] {
            let source = coverage_routing_board(COVERAGE_ROUTING_PAD,"",routing);
            let error = format!("{:#}",export_dsn(Path::new("board.kicad_pcb"),&source,&rules()).unwrap_err());
            assert!(error.contains(expected),"{routing}: {error}");
        }
    }
}
