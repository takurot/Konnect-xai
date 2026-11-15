use crate::mcp::protocol::{CallToolResult, ToolContent};
use crate::tool;
use crate::tools::{
    pcb_board::{attempt_ipc_write, BoardWrite},
    ToolContext, ToolDef,
};
use anyhow::{bail, Context, Result};
use konnect_ipc::gen::kiapi;
use prost::Message;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
#[error("footprint chamfer readback failed")]
struct ChamferReadbackFailed;

#[derive(Debug, Clone)]
struct LibraryFootprint {
    library_id: String,
    definition: kiapi::board::types::Footprint,
    attributes: kiapi::board::types::FootprintAttributes,
    datasheet: Option<String>,
    description_field: Option<String>,
    properties: Vec<kiapi::board::types::Field>,
    pads: Vec<konnect_ipc::IpcPadDefinition>,
    graphics: Vec<konnect_ipc::IpcGraphicDefinition>,
    models: Vec<kiapi::board::types::Footprint3DModel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum ChangedDomain {
    Text,
    Pads,
    Graphics,
    Attributes,
    Metadata,
    Models,
}

#[derive(Debug)]
struct PreparedUpdate {
    item: prost_types::Any,
    changed_domains: BTreeSet<ChangedDomain>,
    preserved: PreservedState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct UpdateFilters {
    reset_text: bool,
    references: Option<BTreeSet<String>>,
    library_ids: Option<BTreeSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArgumentError {
    field: String,
    reason: String,
}

impl std::fmt::Display for ArgumentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.field, self.reason)
    }
}

impl std::error::Error for ArgumentError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PlanStatus {
    Ready,
    Noop,
    Conflict,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct CountPair {
    planned: usize,
    applied: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct UpdateCoverage {
    selected: CountPair,
    changed: CountPair,
    unchanged: CountPair,
    skipped_unlinked: CountPair,
    conflicts: CountPair,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PreservedState {
    position: bool,
    rotation: bool,
    layer: bool,
    locked: bool,
    kiid: bool,
    symbol_path: bool,
    pad_nets: bool,
    instance_overrides: bool,
}

impl PreservedState {
    /// Derived by comparing the rebuilt instance against the one read from
    /// the board — never asserted from policy. If a future change to
    /// `build_updated_instance` stops carrying one of these across, the
    /// response says so instead of echoing the intent.
    fn derive(
        current: &kiapi::board::types::FootprintInstance,
        updated: &kiapi::board::types::FootprintInstance,
        old_nets: &BTreeMap<String, kiapi::board::types::Net>,
    ) -> Self {
        let new_nets: BTreeMap<String, String> = updated
            .definition
            .iter()
            .flat_map(|definition| definition.items.iter())
            .filter(|item| item.type_url.ends_with("kiapi.board.types.Pad"))
            .filter_map(|item| kiapi::board::types::Pad::decode(item.value.as_slice()).ok())
            .filter_map(|pad| pad.net.map(|net| (pad.number, net.name)))
            .collect();
        Self {
            position: updated.position == current.position,
            rotation: updated.orientation == current.orientation,
            layer: updated.layer == current.layer,
            locked: updated.locked == current.locked,
            kiid: updated.id == current.id,
            symbol_path: updated.symbol_path == current.symbol_path
                && updated.symbol_sheet_name == current.symbol_sheet_name
                && updated.symbol_sheet_filename == current.symbol_sheet_filename,
            pad_nets: old_nets.iter().all(|(number, net)| {
                new_nets.get(number).map(String::as_str) == Some(net.name.as_str())
            }),
            instance_overrides: updated.overrides == current.overrides,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PlannedUpdate {
    reference: String,
    library_id: String,
    changed_domains: BTreeSet<ChangedDomain>,
    preserved: PreservedState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct UpdateDiagnostic {
    code: String,
    message: String,
    reference: Option<String>,
}

#[derive(Debug)]
struct UpdatePlan {
    status: PlanStatus,
    plan_revision: String,
    coverage: UpdateCoverage,
    changes: Vec<PlannedUpdate>,
    diagnostics: Vec<UpdateDiagnostic>,
    prepared_items: Vec<prost_types::Any>,
}

pub(crate) fn tool() -> ToolDef {
    tool!(
        "update_footprints_from_library",
        "Plan or atomically apply KiCad's Update Footprints from Library operation to placed \
         footprints on the live board. Defaults to a non-mutating dry run; apply requires its \
         exact plan revision. Preserves placed-instance state and pad nets while refreshing \
         supported library-owned content.",
        json!({
            "type": "object",
            "properties": {
                "board": {
                    "type": "string",
                    "description": "Open .kicad_pcb path whose placed footprints will be refreshed"
                },
                "references": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional exact reference allowlist; omitted means all eligible footprints"
                },
                "library_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional exact Library:Footprint allowlist"
                },
                "reset_text": {
                    "type": "boolean", "default": false,
                    "description": "Restore Reference and Value presentation from the library while retaining their placed text"
                },
                "dry_run": {
                    "type": "boolean",
                    "default": true,
                    "description": "Plan and report changes without mutating the board"
                },
                "expected_plan_revision": {
                    "type": "string",
                    "description": "Required for apply; exact revision returned by the reviewed dry run"
                }
            },
            "required": ["board"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_update_footprints_from_library(args, ctx).await }
    )
    .with_board_access(crate::tools::BoardAccess::LiveOnly)
}

pub(crate) async fn handle_update_footprints_from_library(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> Result<CallToolResult> {
    use kiapi::common::types::KiCadObjectType as ObjectType;

    let board_path = crate::tools::get_path(args, "board")?;
    let dry_run = args["dry_run"].as_bool().unwrap_or(true);
    let expected_revision = args["expected_plan_revision"].as_str().map(str::to_string);
    if !dry_run && expected_revision.is_none() {
        return Ok(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::InvalidArgument {
                field: "expected_plan_revision".to_string(),
                reason: "required when dry_run is false".to_string(),
            },
            "Apply requires the plan revision returned by a current dry run.",
        ));
    }
    if !board_path.is_file() {
        return Ok(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::FileNotFound {
                path: board_path.display().to_string(),
            },
            format!("Board file not found: {}", board_path.display()),
        ));
    }
    let filters = match parse_filters(args) {
        Ok(filters) => filters,
        Err(error) => {
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::InvalidArgument {
                    field: error.field.clone(),
                    reason: error.reason.clone(),
                },
                format!("Argument '{}' is invalid: {}", error.field, error.reason),
            ))
        }
    };

    let ipc_board_path = board_path.clone();
    let planning_board_path = board_path.clone();
    let operation = if dry_run {
        "footprint library update dry run"
    } else {
        "footprint library update apply"
    };
    let result = attempt_ipc_write(
        ctx,
        &board_path,
        operation,
        move |client| {
            let document = client.find_open_board(&ipc_board_path)?;
            let footprint_items =
                client.get_items_in(document.clone(), ObjectType::KotPcbFootprint)?;
            let nets = client.get_nets_in(document.clone())?;
            let net_codes = nets
                .into_iter()
                .map(|net| (net.name, net.netcode))
                .collect::<BTreeMap<_, _>>();
            let routed_nets = snapshot_routed_nets(client, document.clone())?;
            let copper_layer_count = client
                .get_enabled_layers_in(document.clone())?
                .copper_layer_count as usize;
            anyhow::ensure!(
                (2..=32).contains(&copper_layer_count) && copper_layer_count.is_multiple_of(2),
                "KiCad returned an unsupported copper layer count {copper_layer_count} for the requested board"
            );
            let mut plan = plan_updates(
                &planning_board_path,
                &footprint_items,
                &net_codes,
                &routed_nets,
                &filters,
                copper_layer_count,
            );

            if !plan.prepared_items.is_empty() {
                let source = client.save_document_to_string_in(document.clone())?;
                let blocked = super::library_footprint::live_loss_diagnostics(&source)?;
                let mut hasher = Sha256::new();
                hasher.update(plan.plan_revision.as_bytes());
                hasher.update(serde_json::to_vec(&blocked).expect("loss findings serialize"));
                plan.plan_revision = format!("{:x}", hasher.finalize());
                for item in &plan.prepared_items {
                    let instance = kiapi::board::types::FootprintInstance::decode(item.value.as_slice())?;
                    let id = instance.id.as_ref().context("updated footprint has no UUID")?;
                    if let Some(reason) = blocked.get(&id.value) {
                        let reference = field_text(&instance.reference_field);
                        plan.diagnostics.push(UpdateDiagnostic { code: "unsupported_live_footprint_state".into(), message: format!("{reference}: {reason}"), reference: Some(reference) });
                    }
                }
                if !plan.diagnostics.is_empty() {
                    plan.status = PlanStatus::Conflict;
                    plan.coverage.conflicts.planned = plan.diagnostics.len();
                    plan.prepared_items.clear();
                    plan.changes.clear();
                    plan.coverage.changed.planned = 0;
                }
            }
            if dry_run || plan.status == PlanStatus::Conflict {
                return Ok(plan_response(&plan, false));
            }
            if expected_revision.as_deref() != Some(plan.plan_revision.as_str()) {
                plan.status = PlanStatus::Conflict;
                plan.coverage.conflicts.planned += 1;
                plan.diagnostics.push(UpdateDiagnostic {
                    code: "stale_plan_revision".to_string(),
                    message: "The live board, selected libraries, or filters changed; rerun dry run and apply its new plan revision."
                        .to_string(),
                    reference: None,
                });
                plan.changes.clear();
                plan.prepared_items.clear();
                return Ok(plan_response(&plan, false));
            }
            if plan.status == PlanStatus::Noop {
                return Ok(plan_response(&plan, false));
            }

            let update_count = plan.prepared_items.len();
            let presentation = konnect_ipc::FootprintPresentation::from_items(&plan.prepared_items)?;
            let expected_layers = footprint_layer_states(&plan.prepared_items)?;
            let chamfers = konnect_ipc::FootprintChamfers::from_items(&plan.prepared_items)?;
            let mutation = client.run_commit("Update footprints from libraries", |client| {
                client.update_items_in(document.clone(), std::mem::take(&mut plan.prepared_items))?;
                let observed = client.get_items_in(document.clone(), ObjectType::KotPcbFootprint);
                let observed = if chamfers.is_empty() {
                    observed?
                } else {
                    observed.context(ChamferReadbackFailed)?
                };
                if !chamfers.is_empty() {
                    chamfers.verify(&observed).context(ChamferReadbackFailed)?;
                }
                verify_footprint_layer_readback(&expected_layers, &observed)
            });
            if let Err(error) = mutation {
                if error.downcast_ref::<ChamferReadbackFailed>().is_some()
                    && error.downcast_ref::<konnect_ipc::client::IpcOutcomeUnknown>().is_none()
                    && error.downcast_ref::<konnect_ipc::client::IpcBatchRecovered>().is_none()
                {
                    return Ok(CallToolResult::error_kind(
                        crate::mcp::error::ToolErrorKind::HandlerError { reason: format!("{error:#}") },
                        format!("Footprint library update for '{}' was rolled back after chamfer readback failed: {error:#}. Rerun dry run before retrying.", planning_board_path.display()),
                    ));
                }
                return Err(error);
            }
            client.verify_footprint_presentation_in(document, &presentation)?;
            plan.coverage.selected.applied = plan.coverage.selected.planned;
            plan.coverage.changed.applied = update_count;
            plan.coverage.unchanged.applied = plan.coverage.unchanged.planned;
            plan.coverage.skipped_unlinked.applied = plan.coverage.skipped_unlinked.planned;
            Ok(plan_response(&plan, true))
        },
    )
    .await?;

    Ok(match result {
        BoardWrite::Ipc(response) => response,
        BoardWrite::File(reason) => preflight_conflict(format!(
            "{} update_footprints_from_library is live-IPC-only and never edits the board \
             file directly. Open the requested board in KiCad and retry.",
            reason.premise()
        )),
        BoardWrite::Refused(result) => {
            if matches!(
                crate::mcp::error::extract_error_kind(&result).as_deref(),
                Some("ipc_outcome_unknown" | "ipc_batch_recovered")
            ) {
                return Ok(result);
            }
            let message = result
                .content
                .into_iter()
                .find_map(|content| match content {
                    ToolContent::Text { text } => Some(text),
                    _ => None,
                })
                .unwrap_or_else(|| "KiCad refused the footprint library update".to_string());
            preflight_conflict(message)
        }
    })
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum LayerState {
    Footprint(i32),
    Pad(Vec<u8>),
    Graphic(Vec<u8>),
    Text(Vec<u8>),
    Field { name: String, geometry: Vec<u8> },
}

fn normalized_layer_text(text: &kiapi::board::types::BoardText) -> Result<Vec<u8>> {
    let mut text = text.clone();
    text.id = None;
    text.parent = None;
    text.locked = kiapi::common::types::LockedState::LsUnlocked as i32;
    let content = text
        .text
        .as_mut()
        .context("footprint readback text has no content")?;
    if let Some(attributes) = content.attributes.as_mut() {
        if let Some(angle) = attributes.angle.as_mut() {
            anyhow::ensure!(
                angle.value_degrees.is_finite(),
                "footprint readback text has invalid rotation"
            );
            angle.value_degrees =
                konnect_sexp::geometry::round6(angle.value_degrees.rem_euclid(360.0));
            if angle.value_degrees == 0.0 {
                attributes.angle = None;
            }
        }
        // KiCad ignores the deprecated visible bit for BoardText; Fields own visibility.
        attributes.visible = false;
        if !content.text.contains('\n') {
            attributes.multiline = false;
        }
    }
    Ok(text.encode_to_vec())
}

fn text_layer_state(text: &kiapi::board::types::BoardText) -> Result<LayerState> {
    Ok(LayerState::Text(normalized_layer_text(text)?))
}

fn append_field_layer_state(
    states: &mut Vec<LayerState>,
    name: &str,
    field: &kiapi::board::types::Field,
) -> Result<()> {
    let text = field
        .text
        .as_ref()
        .context("footprint readback field has no text")?;
    states.push(LayerState::Field {
        name: name.to_string(),
        geometry: normalized_layer_text(text)?,
    });
    Ok(())
}

fn pad_layer_state(mut pad: kiapi::board::types::Pad) -> Result<LayerState> {
    let stack = pad
        .pad_stack
        .as_mut()
        .context("footprint readback pad has no padstack")?;
    if let Some(angle) = stack.angle.as_mut() {
        anyhow::ensure!(
            angle.value_degrees.is_finite(),
            "footprint readback pad has invalid rotation"
        );
        angle.value_degrees = konnect_sexp::geometry::round6(angle.value_degrees.rem_euclid(360.0));
        if angle.value_degrees == 0.0 {
            stack.angle = None;
        }
    }
    for layer in &mut stack.copper_layers {
        anyhow::ensure!(
            layer.corner_rounding_ratio.is_finite() && layer.chamfer_ratio.is_finite(),
            "footprint readback pad has an invalid corner ratio"
        );
        layer.corner_rounding_ratio += 0.0;
        layer.chamfer_ratio += 0.0;
        if layer
            .offset
            .as_ref()
            .is_some_and(|offset| offset.x_nm == 0 && offset.y_nm == 0)
        {
            layer.offset = None;
        }
    }
    stack
        .copper_layers
        .sort_by_key(prost::Message::encode_to_vec);
    pad.parent = None;
    pad.locked = kiapi::common::types::LockedState::LsUnlocked as i32;
    let definition = kiapi::board::types::Footprint {
        items: vec![konnect_ipc::builders::pack_any(
            &pad,
            "kiapi.board.types.Pad",
        )],
        ..Default::default()
    };
    Ok(LayerState::Pad(
        normalized_items(&definition, "Pad")?
            .pop()
            .context("footprint readback pad has no geometry")?,
    ))
}

fn footprint_layer_state(
    instance: &kiapi::board::types::FootprintInstance,
) -> Result<Vec<LayerState>> {
    let definition = instance
        .definition
        .as_ref()
        .context("footprint readback has no definition")?;
    let mut states = vec![LayerState::Footprint(instance.layer)];
    for (name, field) in [
        ("Reference", &instance.reference_field),
        ("Value", &instance.value_field),
        ("Datasheet", &instance.datasheet_field),
        ("Description", &instance.description_field),
        ("definition.Reference", &definition.reference_field),
        ("definition.Value", &definition.value_field),
        ("definition.Datasheet", &definition.datasheet_field),
        ("definition.Description", &definition.description_field),
    ] {
        if let Some(field) = field {
            append_field_layer_state(&mut states, name, field)?;
        }
    }
    for item in &definition.items {
        if konnect_ipc::builders::any_is(item, "kiapi.board.types.Pad") {
            states.push(pad_layer_state(kiapi::board::types::Pad::decode(
                item.value.as_slice(),
            )?)?);
        } else if konnect_ipc::builders::any_is(item, "kiapi.board.types.BoardGraphicShape") {
            let mut graphic =
                kiapi::board::types::BoardGraphicShape::decode(item.value.as_slice())?;
            graphic.parent = None;
            if let Some(fill) = graphic
                .shape
                .as_mut()
                .and_then(|shape| shape.attributes.as_mut())
                .and_then(|attributes| attributes.fill.as_mut())
            {
                fill.color = None;
            }
            let definition = kiapi::board::types::Footprint {
                items: vec![konnect_ipc::builders::pack_any(
                    &graphic,
                    "kiapi.board.types.BoardGraphicShape",
                )],
                ..Default::default()
            };
            states.push(LayerState::Graphic(
                normalized_graphics(&definition)?
                    .pop()
                    .context("footprint readback graphic has no geometry")?,
            ));
        } else if konnect_ipc::builders::any_is(item, "kiapi.board.types.BoardText") {
            states.push(text_layer_state(&kiapi::board::types::BoardText::decode(
                item.value.as_slice(),
            )?)?);
        } else if konnect_ipc::builders::any_is(item, "kiapi.board.types.Field") {
            let field = kiapi::board::types::Field::decode(item.value.as_slice())?;
            append_field_layer_state(&mut states, &format!("custom.{}", field.name), &field)?;
        }
    }
    states.sort();
    Ok(states)
}

fn footprint_layer_states(items: &[prost_types::Any]) -> Result<BTreeMap<String, Vec<LayerState>>> {
    let mut states = BTreeMap::new();
    for item in items {
        anyhow::ensure!(
            konnect_ipc::builders::any_is(item, "kiapi.board.types.FootprintInstance"),
            "footprint layer readback returned an unexpected item type"
        );
        let instance = kiapi::board::types::FootprintInstance::decode(item.value.as_slice())?;
        let id = instance
            .id
            .as_ref()
            .map(|id| id.value.as_str())
            .filter(|id| !id.is_empty())
            .context("footprint layer readback has no KIID")?;
        anyhow::ensure!(
            states
                .insert(id.to_string(), footprint_layer_state(&instance)?)
                .is_none(),
            "footprint layer readback returned duplicate KIID '{id}'"
        );
    }
    Ok(states)
}

fn verify_footprint_layer_readback(
    expected: &BTreeMap<String, Vec<LayerState>>,
    observed: &[prost_types::Any],
) -> Result<()> {
    let mut selected = Vec::new();
    for item in observed {
        anyhow::ensure!(
            konnect_ipc::builders::any_is(item, "kiapi.board.types.FootprintInstance"),
            "footprint layer readback returned an unexpected item type"
        );
        let instance = kiapi::board::types::FootprintInstance::decode(item.value.as_slice())?;
        if instance
            .id
            .as_ref()
            .is_some_and(|id| expected.contains_key(&id.value))
        {
            selected.push(item.clone());
        }
    }
    let observed = footprint_layer_states(&selected)?;
    for (id, layers) in expected {
        anyhow::ensure!(
            observed.get(id) == Some(layers),
            "footprint '{id}' layer or mirrored-text readback differs from the reviewed update"
        );
    }
    Ok(())
}

fn snapshot_routed_nets(
    client: &konnect_ipc::KiCadIpcClient,
    document: kiapi::common::types::DocumentSpecifier,
) -> Result<BTreeSet<String>> {
    use kiapi::common::types::KiCadObjectType as ObjectType;

    let mut routed = BTreeSet::new();
    for item in client.get_items_in(document.clone(), ObjectType::KotPcbTrace)? {
        if let Ok(track) = kiapi::board::types::Track::decode(item.value.as_slice()) {
            if let Some(net) = track.net.filter(|net| !net.name.is_empty()) {
                routed.insert(net.name);
            }
        }
    }
    for item in client.get_items_in(document.clone(), ObjectType::KotPcbArc)? {
        if let Ok(arc) = kiapi::board::types::Arc::decode(item.value.as_slice()) {
            if let Some(net) = arc.net.filter(|net| !net.name.is_empty()) {
                routed.insert(net.name);
            }
        }
    }
    for item in client.get_items_in(document.clone(), ObjectType::KotPcbVia)? {
        if let Ok(via) = kiapi::board::types::Via::decode(item.value.as_slice()) {
            if let Some(net) = via.net.filter(|net| !net.name.is_empty()) {
                routed.insert(net.name);
            }
        }
    }
    for item in client.get_items_in(document, ObjectType::KotPcbZone)? {
        if let Some(net) = super::pcb_zone_nets::routed_net(&item)? {
            routed.insert(net.name);
        }
    }
    Ok(routed)
}

#[cfg(test)]
mod zone_net_tests {
    use super::*;
    use crate::tools::pcb_zone_nets::test_support::{
        captured_zones, copper, invalid_zones, mock_zones, rule_area,
        serializer_shaped_graphical_zones,
    };

    #[test]
    fn graphical_zone_does_not_block_the_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let board = directory.path().join("zones.kicad_pcb");
        for zone in serializer_shaped_graphical_zones() {
            let (server, _) = mock_zones(&board, vec![zone]);
            let client = konnect_ipc::KiCadIpcClient::new(server.address().to_string());
            let document = client.find_open_board(&board).unwrap();
            assert!(snapshot_routed_nets(&client, document).unwrap().is_empty());
        }
    }

    #[test]
    fn zone_snapshot_counts_only_its_copper_net() {
        let directory = tempfile::tempdir().unwrap();
        let board = directory.path().join("zones.kicad_pcb");
        for (zones, expected) in [
            (vec![copper("GND", 1)], BTreeSet::from(["GND".to_string()])),
            (vec![rule_area()], BTreeSet::new()),
            (vec![copper("", 0)], BTreeSet::new()),
            (captured_zones(), BTreeSet::from(["GND".to_string()])),
        ] {
            let (server, _) = mock_zones(&board, zones);
            let client = konnect_ipc::KiCadIpcClient::new(server.address().to_string());
            let document = client.find_open_board(&board).unwrap();
            assert_eq!(snapshot_routed_nets(&client, document).unwrap(), expected);
        }
    }

    #[test]
    fn zone_snapshot_refuses_unknown_copper_net_before_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let board = directory.path().join("zones.kicad_pcb");
        for zone in invalid_zones() {
            let (server, requests) = mock_zones(&board, vec![zone]);
            let client = konnect_ipc::KiCadIpcClient::new(server.address().to_string());
            let document = client.find_open_board(&board).unwrap();
            let error = snapshot_routed_nets(&client, document).unwrap_err();
            assert!(error.to_string().contains("zone"), "{error:#}");
            assert!(!requests.lock().unwrap().iter().any(|request| {
                request.ends_with("BeginCommit") || request.ends_with("UpdateItems")
            }));
        }
    }
}

fn plan_response(plan: &UpdatePlan, applied: bool) -> CallToolResult {
    CallToolResult::json(&json!({
        "status": if applied {
            "applied"
        } else {
            match plan.status {
                PlanStatus::Ready => "ready",
                PlanStatus::Noop => "noop",
                PlanStatus::Conflict => "conflict",
            }
        },
        "plan_revision": plan.plan_revision,
        "coverage": {
            "transport": "live_kicad_ipc",
            "atomicity": "single_kicad_undo_commit",
            "selected": plan.coverage.selected,
            "changed": plan.coverage.changed,
            "unchanged": plan.coverage.unchanged,
            "skipped_unlinked": plan.coverage.skipped_unlinked,
            "conflicts": plan.coverage.conflicts
        },
        "changes": plan.changes,
        "diagnostics": plan.diagnostics,
        "undo": if applied {
            Some("Ctrl-Z reverses the whole footprint library update.")
        } else {
            None
        }
    }))
}

fn preflight_conflict(message: impl Into<String>) -> CallToolResult {
    CallToolResult::json(&json!({
        "status": "conflict",
        "coverage": {
            "transport": "live_kicad_ipc",
            "atomicity": "single_kicad_undo_commit",
            "selected": CountPair::default(),
            "changed": CountPair::default(),
            "unchanged": CountPair::default(),
            "skipped_unlinked": CountPair::default(),
            "conflicts": CountPair { planned: 1, applied: 0 }
        },
        "changes": [],
        "diagnostics": [{
            "code": "preflight_conflict",
            "message": message.into()
        }],
        "undo": null
    }))
}

fn parse_filters(args: &serde_json::Value) -> std::result::Result<UpdateFilters, ArgumentError> {
    fn parse(
        args: &serde_json::Value,
        field: &str,
    ) -> std::result::Result<Option<BTreeSet<String>>, ArgumentError> {
        let Some(value) = args.get(field) else {
            return Ok(None);
        };
        let values = value.as_array().ok_or_else(|| ArgumentError {
            field: field.to_string(),
            reason: "must be an array of strings when supplied".to_string(),
        })?;
        let parsed = values
            .iter()
            .map(|value| {
                let value = value.as_str().ok_or_else(|| ArgumentError {
                    field: field.to_string(),
                    reason: "must contain only strings".to_string(),
                })?;
                if value.is_empty() {
                    return Err(ArgumentError {
                        field: field.to_string(),
                        reason: "must not contain empty strings".to_string(),
                    });
                }
                Ok(value.to_string())
            })
            .collect::<std::result::Result<BTreeSet<_>, _>>()
            .map(Some)?;
        if field == "library_ids" {
            if let Some(invalid) = parsed.as_ref().and_then(|values| {
                values.iter().find(|value| {
                    value.split_once(':').is_none_or(|(nickname, entry)| {
                        nickname.is_empty()
                            || entry.is_empty()
                            || value.contains('/')
                            || value.contains('\\')
                    })
                })
            }) {
                return Err(ArgumentError {
                    field: field.to_string(),
                    reason: format!(
                        "'{invalid}' must use a non-empty Library:Footprint identifier"
                    ),
                });
            }
        }
        Ok(parsed)
    }

    let reset_text = match args.get("reset_text") {
        None => false,
        Some(value) => value.as_bool().ok_or_else(|| ArgumentError {
            field: "reset_text".into(),
            reason: "must be a boolean".into(),
        })?,
    };
    Ok(UpdateFilters {
        reset_text,
        references: parse(args, "references")?,
        library_ids: parse(args, "library_ids")?,
    })
}

fn plan_updates(
    board_path: &Path,
    footprint_items: &[prost_types::Any],
    net_codes: &BTreeMap<String, i32>,
    routed_nets: &BTreeSet<String>,
    filters: &UpdateFilters,
    copper_layer_count: usize,
) -> UpdatePlan {
    #[derive(Debug)]
    struct Candidate {
        reference: String,
        library_id: String,
        item: prost_types::Any,
        instance: kiapi::board::types::FootprintInstance,
    }

    let mut coverage = UpdateCoverage::default();
    let mut diagnostics = Vec::new();
    let mut candidates = Vec::new();
    let mut reference_counts = BTreeMap::<String, usize>::new();

    for item in footprint_items {
        let instance = match kiapi::board::types::FootprintInstance::decode(item.value.as_slice()) {
            Ok(instance) => instance,
            Err(error) => {
                diagnostics.push(UpdateDiagnostic {
                    code: "invalid_board_footprint".to_string(),
                    message: format!("KiCad returned an invalid footprint: {error}"),
                    reference: None,
                });
                continue;
            }
        };
        let reference = field_text(&instance.reference_field);
        *reference_counts.entry(reference.clone()).or_insert(0) += 1;
        let library_id = instance
            .definition
            .as_ref()
            .and_then(|definition| definition.id.as_ref())
            .map(|id| format!("{}:{}", id.library_nickname, id.entry_name))
            .unwrap_or_default();
        candidates.push(Candidate {
            reference,
            library_id,
            item: item.clone(),
            instance,
        });
    }

    if let Some(references) = filters.references.as_ref() {
        for reference in references {
            match reference_counts.get(reference).copied().unwrap_or(0) {
                0 => diagnostics.push(UpdateDiagnostic {
                    code: "reference_not_found".to_string(),
                    message: format!("footprint reference '{reference}' was not found"),
                    reference: Some(reference.clone()),
                }),
                count if count > 1 => diagnostics.push(UpdateDiagnostic {
                    code: "duplicate_reference".to_string(),
                    message: format!("footprint reference '{reference}' appears {count} times"),
                    reference: Some(reference.clone()),
                }),
                _ => {}
            }
        }
    } else {
        for (reference, count) in &reference_counts {
            if *count > 1 {
                diagnostics.push(UpdateDiagnostic {
                    code: "duplicate_reference".to_string(),
                    message: format!("footprint reference '{reference}' appears {count} times"),
                    reference: Some(reference.clone()),
                });
            }
        }
    }

    candidates.sort_by(|left, right| {
        left.reference
            .cmp(&right.reference)
            .then_with(|| left.library_id.cmp(&right.library_id))
    });
    let selected = candidates
        .into_iter()
        .filter(|candidate| {
            filters
                .references
                .as_ref()
                .is_none_or(|references| references.contains(&candidate.reference))
        })
        .filter(|candidate| {
            filters
                .library_ids
                .as_ref()
                .is_none_or(|ids| ids.contains(&candidate.library_id))
        })
        .collect::<Vec<_>>();

    coverage.skipped_unlinked.planned = selected
        .iter()
        .filter(|candidate| candidate.library_id.is_empty())
        .count();
    coverage.selected.planned = selected
        .iter()
        .filter(|candidate| !candidate.library_id.is_empty())
        .count();
    if filters.references.is_some() {
        for candidate in selected
            .iter()
            .filter(|candidate| candidate.library_id.is_empty())
        {
            diagnostics.push(UpdateDiagnostic {
                code: "unlinked_footprint".to_string(),
                message: format!(
                    "footprint '{}' has no resolvable Library:Footprint identifier",
                    candidate.reference
                ),
                reference: Some(candidate.reference.clone()),
            });
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(filters).expect("filters serialize"));
    hasher.update(copper_layer_count.to_le_bytes());
    let mut changes = Vec::new();
    let mut prepared_items = Vec::new();
    for candidate in selected {
        if candidate.library_id.is_empty() {
            continue;
        }
        let Some((nickname, entry)) = candidate.library_id.split_once(':') else {
            diagnostics.push(UpdateDiagnostic {
                code: "invalid_library_id".to_string(),
                message: format!(
                    "footprint '{}' has malformed library id '{}'",
                    candidate.reference, candidate.library_id
                ),
                reference: Some(candidate.reference),
            });
            continue;
        };
        if nickname.is_empty() || entry.is_empty() {
            diagnostics.push(UpdateDiagnostic {
                code: "invalid_library_id".to_string(),
                message: format!(
                    "footprint '{}' has malformed library id '{}'",
                    candidate.reference, candidate.library_id
                ),
                reference: Some(candidate.reference),
            });
            continue;
        }

        let library_path = match super::library::resolve_footprint_path(
            &candidate.library_id,
            board_path.parent(),
        ) {
            Ok(path) => path,
            Err(message) => {
                diagnostics.push(UpdateDiagnostic {
                    code: "footprint_library_resolution_failed".to_string(),
                    message,
                    reference: Some(candidate.reference),
                });
                continue;
            }
        };
        let source = match std::fs::read_to_string(&library_path) {
            Ok(source) => source,
            Err(error) => {
                diagnostics.push(UpdateDiagnostic {
                    code: "footprint_library_read_failed".to_string(),
                    message: format!("failed to read {}: {error}", library_path.display()),
                    reference: Some(candidate.reference),
                });
                continue;
            }
        };
        let library = match parse_library_footprint(&candidate.library_id, &source) {
            Ok(library) => library,
            Err(error) => {
                diagnostics.push(UpdateDiagnostic {
                    code: "unsupported_library_footprint".to_string(),
                    message: format!("{error:#}"),
                    reference: Some(candidate.reference),
                });
                continue;
            }
        };
        let prepared = match build_updated_instance(
            &candidate.instance,
            &library,
            net_codes,
            routed_nets,
            filters.reset_text,
            copper_layer_count,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                diagnostics.push(UpdateDiagnostic {
                    code: "footprint_update_conflict".to_string(),
                    message: format!("{error:#}"),
                    reference: Some(candidate.reference),
                });
                continue;
            }
        };

        hasher.update(candidate.item.type_url.as_bytes());
        hasher.update(&candidate.item.value);
        hasher.update(candidate.library_id.as_bytes());
        hasher.update(source.as_bytes());
        if prepared.changed_domains.is_empty() {
            coverage.unchanged.planned += 1;
        } else {
            coverage.changed.planned += 1;
            changes.push(PlannedUpdate {
                reference: candidate.reference,
                library_id: candidate.library_id,
                changed_domains: prepared.changed_domains,
                preserved: prepared.preserved,
            });
            prepared_items.push(prepared.item);
        }
    }

    if !diagnostics.is_empty() {
        coverage.conflicts.planned = diagnostics.len();
        changes.clear();
        prepared_items.clear();
        coverage.changed.planned = 0;
        return UpdatePlan {
            status: PlanStatus::Conflict,
            plan_revision: format!("{:x}", hasher.finalize()),
            coverage,
            changes,
            diagnostics,
            prepared_items,
        };
    }

    let status = if changes.is_empty() {
        PlanStatus::Noop
    } else {
        PlanStatus::Ready
    };
    UpdatePlan {
        status,
        plan_revision: format!("{:x}", hasher.finalize()),
        coverage,
        changes,
        diagnostics,
        prepared_items,
    }
}

fn parse_library_footprint(library_id: &str, source: &str) -> Result<LibraryFootprint> {
    let (library_nickname, entry_name) = library_id
        .split_once(':')
        .filter(|(nickname, entry)| !nickname.is_empty() && !entry.is_empty())
        .context("footprint identifier must use non-empty Library:Footprint syntax")?;
    let root = konnect_sexp::parse_sexp(source).context("invalid footprint S-expression")?;
    if root.head() != Some("footprint") {
        bail!("library source root must be a footprint");
    }

    super::library_footprint::validate_supported_children(&root)?;
    let fields = super::library_footprint::fields(&root)?;
    let value = |name: &str| {
        fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| field_text(&Some(field.clone())))
    };
    let datasheet = value("Datasheet");
    let description_field = value("Description");
    let properties = fields
        .iter()
        .filter(|field| {
            !matches!(
                field.name.as_str(),
                "Reference" | "Value" | "Datasheet" | "Description"
            )
        })
        .cloned()
        .collect();
    let pads = super::pcb_components::extract_pad_definitions(source)?;
    // Custom properties travel as typed Field items below. Treating visible
    // properties as generic graphics as well would duplicate their text.
    let graphics = super::pcb_components::extract_graphic_definitions_without_properties(source)?;
    let models = super::library_footprint::models(&root)?;
    let attributes = super::library_footprint::attributes(&root)?;
    let mut definition = kiapi::board::types::Footprint {
        id: Some(kiapi::common::types::LibraryIdentifier {
            library_nickname: library_nickname.to_string(),
            entry_name: entry_name.to_string(),
        }),
        attributes: Some(super::library_footprint::description_and_keywords(&root)),
        ..Default::default()
    };

    for field in fields {
        match field.name.as_str() {
            "Reference" => definition.reference_field = Some(field),
            "Value" => definition.value_field = Some(field),
            "Datasheet" => definition.datasheet_field = Some(field),
            "Description" => definition.description_field = Some(field),
            _ => {}
        }
    }

    Ok(LibraryFootprint {
        library_id: library_id.to_string(),
        definition,
        attributes,
        datasheet,
        description_field,
        properties,
        pads,
        graphics,
        models,
    })
}

fn build_updated_instance(
    current: &kiapi::board::types::FootprintInstance,
    library: &LibraryFootprint,
    net_codes: &BTreeMap<String, i32>,
    routed_nets: &BTreeSet<String>,
    reset_text: bool,
    copper_layer_count: usize,
) -> Result<PreparedUpdate> {
    let current_definition = current
        .definition
        .as_ref()
        .context("board footprint has no embedded definition")?;
    let current_id = current_definition
        .id
        .as_ref()
        .map(|id| format!("{}:{}", id.library_nickname, id.entry_name))
        .unwrap_or_default();
    if current_id != library.library_id {
        bail!(
            "board footprint library id '{current_id}' does not match '{}'",
            library.library_id
        );
    }

    let mut old_nets = BTreeMap::<String, kiapi::board::types::Net>::new();
    for item in &current_definition.items {
        if !item.type_url.ends_with("kiapi.board.types.Pad") {
            continue;
        }
        let pad = kiapi::board::types::Pad::decode(item.value.as_slice())
            .context("board footprint contains an invalid pad")?;
        let Some(net) = pad.net.filter(|net| !net.name.is_empty()) else {
            continue;
        };
        if let Some(existing) = old_nets.insert(pad.number.clone(), net.clone()) {
            if existing.name != net.name {
                bail!(
                    "logical pad {} carries multiple nets ('{}' and '{}')",
                    pad.number,
                    existing.name,
                    net.name
                );
            }
        }
    }
    let new_numbers = library
        .pads
        .iter()
        .map(|pad| pad.number.as_str())
        .collect::<BTreeSet<_>>();
    for (number, net) in &old_nets {
        if !new_numbers.contains(number.as_str()) {
            let routed = routed_nets.contains(&net.name);
            bail!(
                "library update removes connected pad {number} on net '{}'{}",
                net.name,
                if routed { " with routed copper" } else { "" }
            );
        }
    }

    let position = current.position.as_ref().cloned().unwrap_or_default();
    let rotation = current
        .orientation
        .as_ref()
        .map(|angle| angle.value_degrees)
        .unwrap_or(0.0);
    let is_back = current.layer == kiapi::board::types::BoardLayer::BlBCu as i32;
    let layer = if is_back { "B.Cu" } else { "F.Cu" };
    let (pads, graphics) = if is_back {
        (
            library
                .pads
                .iter()
                .map(|item| mirror_pad(item, copper_layer_count))
                .collect::<Result<Vec<_>>>()?,
            library
                .graphics
                .iter()
                .map(|item| mirror_graphic(item, copper_layer_count))
                .collect::<Result<Vec<_>>>()?,
        )
    } else {
        (library.pads.clone(), library.graphics.clone())
    };
    let packed = konnect_ipc::KiCadIpcClient::build_footprint_item(
        &library.library_id,
        &field_text(&current.reference_field),
        &field_text(&current.value_field),
        &pads,
        &graphics,
        &Default::default(),
        konnect_ipc::builders::nm_to_mm(position.x_nm),
        konnect_ipc::builders::nm_to_mm(position.y_nm),
        rotation,
        layer,
    )?;
    let built = kiapi::board::types::FootprintInstance::decode(packed.value.as_slice())
        .context("typed footprint builder returned an invalid item")?;

    let mut updated = current.clone();
    let mut definition = built
        .definition
        .context("typed footprint builder returned no definition")?;
    definition.attributes = library.definition.attributes.clone();
    definition.reference_field = current_definition.reference_field.clone();
    definition.value_field = current_definition.value_field.clone();
    definition.datasheet_field = current_definition.datasheet_field.clone();
    definition.description_field = current_definition.description_field.clone();
    apply_field_value(
        &mut definition.datasheet_field,
        library.datasheet.as_deref(),
    );
    apply_field_value(
        &mut definition.description_field,
        library.description_field.as_deref(),
    );
    definition.items.extend(
        library.models.iter().map(|model| {
            konnect_ipc::builders::pack_any(model, "kiapi.board.types.Footprint3DModel")
        }),
    );
    for item in &mut definition.items {
        if item.type_url.ends_with("kiapi.board.types.Pad") {
            let mut pad = kiapi::board::types::Pad::decode(item.value.as_slice())?;
            pad.net = old_nets
                .get(&pad.number)
                .map(|old| kiapi::board::types::Net {
                    code: net_codes
                        .get(&old.name)
                        .copied()
                        .or_else(|| old.code.as_ref().map(|code| code.value))
                        .map(|value| kiapi::board::types::NetCode { value }),
                    name: old.name.clone(),
                });
            *item = konnect_ipc::builders::pack_any(&pad, "kiapi.board.types.Pad");
        } else if is_back && item.type_url.ends_with("kiapi.board.types.BoardText") {
            let mut text = kiapi::board::types::BoardText::decode(item.value.as_slice())?;
            if is_side_specific_layer(text.layer) {
                if let Some(attributes) =
                    text.text.as_mut().and_then(|text| text.attributes.as_mut())
                {
                    attributes.mirrored = true;
                }
            }
            *item = konnect_ipc::builders::pack_any(&text, "kiapi.board.types.BoardText");
        }
    }
    merge_custom_properties(
        &mut definition,
        current_definition,
        &library.properties,
        &position,
        rotation,
        is_back,
        copper_layer_count,
    )?;
    updated.definition = Some(definition);
    apply_field_value(&mut updated.datasheet_field, library.datasheet.as_deref());
    apply_field_value(
        &mut updated.description_field,
        library.description_field.as_deref(),
    );
    let mut attributes = library.attributes.clone();
    if let Some(current_attributes) = current.attributes.as_ref() {
        attributes.not_in_schematic = current_attributes.not_in_schematic;
        attributes.do_not_populate = current_attributes.do_not_populate;
    }
    updated.attributes = Some(attributes);

    if reset_text {
        for (name, library_field) in [
            ("Reference", &library.definition.reference_field),
            ("Value", &library.definition.value_field),
        ] {
            let Some(library_field) = library_field else {
                continue;
            };
            let mut field = transform_library_property(
                library_field,
                &position,
                rotation,
                is_back,
                copper_layer_count,
            )?;
            let value = if name == "Reference" {
                field_text(&current.reference_field)
            } else {
                field_text(&current.value_field)
            };
            field
                .text
                .as_mut()
                .context("library field has no board text")?
                .text
                .as_mut()
                .context("library field has no text")?
                .text = value;
            if name == "Reference" {
                updated.reference_field = Some(field.clone());
                updated
                    .definition
                    .as_mut()
                    .context("updated footprint has no definition")?
                    .reference_field = Some(field);
            } else {
                updated.value_field = Some(field.clone());
                updated
                    .definition
                    .as_mut()
                    .context("updated footprint has no definition")?
                    .value_field = Some(field);
            }
        }
    }
    let changed_domains = changed_domains(current, &updated)?;
    let preserved = PreservedState::derive(current, &updated, &old_nets);
    Ok(PreparedUpdate {
        item: konnect_ipc::builders::pack_any(&updated, "kiapi.board.types.FootprintInstance"),
        changed_domains,
        preserved,
    })
}

fn apply_field_value(field: &mut Option<kiapi::board::types::Field>, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    field
        .get_or_insert_with(Default::default)
        .text
        .get_or_insert_with(Default::default)
        .text
        .get_or_insert_with(Default::default)
        .text = value.to_string();
}

fn merge_custom_properties(
    updated: &mut kiapi::board::types::Footprint,
    current: &kiapi::board::types::Footprint,
    library_properties: &[kiapi::board::types::Field],
    footprint_position: &kiapi::common::types::Vector2,
    footprint_rotation: f64,
    is_back: bool,
    copper_layer_count: usize,
) -> Result<()> {
    let library_names = library_properties
        .iter()
        .map(|field| field.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut current_names = BTreeSet::new();
    for item in current
        .items
        .iter()
        .filter(|item| item.type_url.ends_with("kiapi.board.types.Field"))
    {
        let field = kiapi::board::types::Field::decode(item.value.as_slice())
            .context("board footprint contains an invalid custom property")?;
        if field.name.is_empty() {
            bail!("board footprint contains a custom property without a name");
        }
        if !current_names.insert(field.name.clone()) {
            bail!(
                "board footprint contains more than one custom property named '{}'",
                field.name
            );
        }
        if !library_names.contains(field.name.as_str()) {
            // A field that exists only on the placed instance belongs to that
            // instance. Preserve its complete typed representation verbatim.
            updated.items.push(item.clone());
        }
    }
    for property in library_properties {
        let property = transform_library_property(
            property,
            footprint_position,
            footprint_rotation,
            is_back,
            copper_layer_count,
        )?;
        updated.items.push(konnect_ipc::builders::pack_any(
            &property,
            "kiapi.board.types.Field",
        ));
    }
    Ok(())
}

fn transform_library_property(
    property: &kiapi::board::types::Field,
    footprint_position: &kiapi::common::types::Vector2,
    footprint_rotation: f64,
    is_back: bool,
    copper_layer_count: usize,
) -> Result<kiapi::board::types::Field> {
    let mut property = property.clone();
    property.id = None;
    let board_text = property
        .text
        .as_mut()
        .with_context(|| format!("property '{}' has no board text", property.name))?;
    board_text.id = None;
    let text = board_text
        .text
        .as_mut()
        .with_context(|| format!("property '{}' has no text value", property.name))?;
    let local_position = text
        .position
        .as_ref()
        .with_context(|| format!("property '{}' has no position", property.name))?;
    let local_x = konnect_ipc::builders::nm_to_mm(local_position.x_nm);
    let mut local_y = konnect_ipc::builders::nm_to_mm(local_position.y_nm);
    if is_back {
        local_y = -local_y;
    }
    let (board_x, board_y) = konnect_sexp::geometry::transform_pad(
        local_x,
        local_y,
        konnect_ipc::builders::nm_to_mm(footprint_position.x_nm),
        konnect_ipc::builders::nm_to_mm(footprint_position.y_nm),
        footprint_rotation,
    );
    text.position = Some(konnect_ipc::builders::vec2(board_x, board_y)?);

    let attributes = text
        .attributes
        .as_mut()
        .with_context(|| format!("property '{}' has no text attributes", property.name))?;
    let local_angle = attributes
        .angle
        .as_ref()
        .map(|angle| angle.value_degrees)
        .unwrap_or(0.0);
    let local_angle = if is_back {
        180.0 - local_angle
    } else {
        local_angle
    };
    let field_rotation = local_angle + footprint_rotation;
    anyhow::ensure!(
        field_rotation.is_finite(),
        "property '{}' rotation must remain finite",
        property.name
    );
    attributes.angle = Some(kiapi::common::types::Angle {
        value_degrees: readable_property_angle(field_rotation),
    });
    if is_back {
        if is_side_specific_layer(board_text.layer) {
            attributes.mirrored = !attributes.mirrored;
        }
        let layer = kiapi::board::types::BoardLayer::try_from(board_text.layer)
            .with_context(|| format!("property '{}' has an invalid layer", property.name))?;
        let layer_name = konnect_ipc::builders::layer_name(layer)
            .with_context(|| format!("property '{}' has an unnamed layer", property.name))?;
        board_text.layer = konnect_ipc::builders::try_layer_from_name(
            &konnect_ipc::builders::flip_layer(layer_name, copper_layer_count)?,
        )? as i32;
    }
    Ok(property)
}

fn readable_property_angle(degrees: f64) -> f64 {
    let mut angle = degrees.rem_euclid(360.0);
    if angle > 90.0 && angle <= 270.0 {
        angle -= 180.0;
    }
    angle
}

fn mirror_pad(
    pad: &konnect_ipc::IpcPadDefinition,
    copper_layer_count: usize,
) -> Result<konnect_ipc::IpcPadDefinition> {
    let mut mirrored = pad.clone();
    mirrored.y = -mirrored.y;
    mirrored.rotation = -mirrored.rotation;
    if let Some(corners) = mirrored.chamfered_corners.as_mut() {
        std::mem::swap(&mut corners.top_left, &mut corners.bottom_left);
        std::mem::swap(&mut corners.top_right, &mut corners.bottom_right);
    }
    mirrored.layers = mirrored
        .layers
        .iter()
        .map(|layer| konnect_ipc::builders::flip_layer(layer, copper_layer_count))
        .collect::<Result<_>>()?;
    Ok(mirrored)
}

fn mirror_graphic(
    graphic: &konnect_ipc::IpcGraphicDefinition,
    copper_layer_count: usize,
) -> Result<konnect_ipc::IpcGraphicDefinition> {
    use konnect_ipc::IpcGraphicDefinition as Graphic;

    let point = |(x, y): (f64, f64)| (x, -y);
    Ok(match graphic {
        Graphic::Line {
            style,
            start,
            end,
            layer,
            width,
        } => Graphic::Line {
            style: *style,
            start: point(*start),
            end: point(*end),
            layer: konnect_ipc::builders::flip_layer(layer, copper_layer_count)?,
            width: *width,
        },
        Graphic::Rect {
            style,
            start,
            end,
            layer,
            width,
            filled,
        } => Graphic::Rect {
            style: *style,
            start: point(*start),
            end: point(*end),
            layer: konnect_ipc::builders::flip_layer(layer, copper_layer_count)?,
            width: *width,
            filled: *filled,
        },
        Graphic::Circle {
            style,
            center,
            end,
            layer,
            width,
            filled,
        } => Graphic::Circle {
            style: *style,
            center: point(*center),
            end: point(*end),
            layer: konnect_ipc::builders::flip_layer(layer, copper_layer_count)?,
            width: *width,
            filled: *filled,
        },
        Graphic::Arc {
            style,
            start,
            mid,
            end,
            layer,
            width,
        } => Graphic::Arc {
            style: *style,
            start: point(*end),
            mid: point(*mid),
            end: point(*start),
            layer: konnect_ipc::builders::flip_layer(layer, copper_layer_count)?,
            width: *width,
        },
        Graphic::Poly {
            style,
            points,
            layer,
            width,
            filled,
        } => Graphic::Poly {
            style: *style,
            points: points.iter().copied().map(point).collect(),
            layer: konnect_ipc::builders::flip_layer(layer, copper_layer_count)?,
            width: *width,
            filled: *filled,
        },
        Graphic::Text {
            text,
            position,
            rotation,
            layer,
            size,
            stroke_width_mm,
        } => Graphic::Text {
            text: text.clone(),
            position: point(*position),
            rotation: 180.0 - rotation,
            layer: konnect_ipc::builders::flip_layer(layer, copper_layer_count)?,
            size: *size,
            stroke_width_mm: *stroke_width_mm,
        },
    })
}

fn is_side_specific_layer(layer: i32) -> bool {
    use kiapi::board::types::BoardLayer;

    if BoardLayer::try_from(layer)
        .ok()
        .and_then(konnect_ipc::builders::layer_name)
        .is_some_and(|name| name.ends_with(".Cu"))
    {
        return true;
    }
    matches!(
        BoardLayer::try_from(layer).ok(),
        Some(
            BoardLayer::BlFAdhes
                | BoardLayer::BlBAdhes
                | BoardLayer::BlFPaste
                | BoardLayer::BlBPaste
                | BoardLayer::BlFSilkS
                | BoardLayer::BlBSilkS
                | BoardLayer::BlFMask
                | BoardLayer::BlBMask
                | BoardLayer::BlFCrtYd
                | BoardLayer::BlBCrtYd
                | BoardLayer::BlFFab
                | BoardLayer::BlBFab
        )
    )
}

fn changed_domains(
    current: &kiapi::board::types::FootprintInstance,
    updated: &kiapi::board::types::FootprintInstance,
) -> Result<BTreeSet<ChangedDomain>> {
    let mut changed = BTreeSet::new();
    let current_definition = current
        .definition
        .as_ref()
        .context("missing current definition")?;
    let updated_definition = updated
        .definition
        .as_ref()
        .context("missing updated definition")?;

    let current_pads = normalized_items(current_definition, "Pad")?;
    let updated_pads = normalized_items(updated_definition, "Pad")?;
    if current_pads != updated_pads {
        changed.insert(ChangedDomain::Pads);
    }
    let current_graphics = normalized_graphics(current_definition)?;
    let updated_graphics = normalized_graphics(updated_definition)?;
    if normalized_field(&current.reference_field)? != normalized_field(&updated.reference_field)?
        || normalized_field(&current.value_field)? != normalized_field(&updated.value_field)?
    {
        changed.insert(ChangedDomain::Text);
    }
    if current_graphics != updated_graphics {
        changed.insert(ChangedDomain::Graphics);
    }
    if normalized_items(current_definition, "Footprint3DModel")?
        != normalized_items(updated_definition, "Footprint3DModel")?
    {
        changed.insert(ChangedDomain::Models);
    }
    if current_definition.attributes != updated_definition.attributes
        || field_text(&current_definition.datasheet_field)
            != field_text(&updated_definition.datasheet_field)
        || field_text(&current_definition.description_field)
            != field_text(&updated_definition.description_field)
        || normalized_items(current_definition, "Field")?
            != normalized_items(updated_definition, "Field")?
    {
        changed.insert(ChangedDomain::Metadata);
    }
    if library_owned_attributes(current.attributes.as_ref())
        != library_owned_attributes(updated.attributes.as_ref())
    {
        changed.insert(ChangedDomain::Attributes);
    }
    Ok(changed)
}

fn normalized_field(
    field: &Option<kiapi::board::types::Field>,
) -> Result<Option<kiapi::board::types::Field>> {
    field
        .as_ref()
        .map(konnect_ipc::normalize_footprint_field)
        .transpose()
}

fn normalized_items(
    definition: &kiapi::board::types::Footprint,
    suffix: &str,
) -> Result<Vec<Vec<u8>>> {
    let mut items = definition
        .items
        .iter()
        .filter(|item| item.type_url.ends_with(suffix))
        .map(|item| {
            if suffix == "Pad" {
                let mut pad = kiapi::board::types::Pad::decode(item.value.as_slice())?;
                pad.id = None;
                pad.net = None;
                pad.pad_to_die_length = None;
                pad.symbol_pin = None;
                pad.pad_to_die_delay = None;
                if let Some(stack) = pad.pad_stack.as_mut() {
                    stack.layers.sort_unstable();
                    for layer in &mut stack.copper_layers {
                        let roundrect = kiapi::board::types::PadStackShape::PssRoundrect as i32;
                        let chamfered = kiapi::board::types::PadStackShape::PssChamferedrect as i32;
                        // KiCad populates defaults even when the shape does not use them.
                        if layer.shape != roundrect && layer.shape != chamfered {
                            layer.corner_rounding_ratio = 0.0;
                        }
                        if layer.shape != chamfered {
                            layer.chamfer_ratio = 0.0;
                            layer.chamfered_corners = None;
                        }
                    }
                    if stack.drill.as_ref().is_some_and(|drill| {
                        drill.start_layer == kiapi::board::types::BoardLayer::BlUndefined as i32
                            && drill.end_layer
                                == kiapi::board::types::BoardLayer::BlUndefined as i32
                            && drill
                                .diameter
                                .as_ref()
                                .is_none_or(|diameter| diameter.x_nm == 0 && diameter.y_nm == 0)
                    }) {
                        stack.drill = None;
                    }
                    if stack.zone_settings.as_ref().is_some_and(|settings| {
                        settings.zone_connection
                            == kiapi::board::types::ZoneConnectionStyle::ZcsInherited as i32
                    }) {
                        stack.zone_settings = None;
                    }
                }
                Ok(pad.encode_to_vec())
            } else if suffix == "Field" {
                let field = kiapi::board::types::Field::decode(item.value.as_slice())?;
                Ok(konnect_ipc::normalize_footprint_field(&field)?.encode_to_vec())
            } else {
                Ok(item.value.clone())
            }
        })
        .collect::<Result<Vec<_>>>()?;
    items.sort();
    Ok(items)
}

fn normalized_graphics(definition: &kiapi::board::types::Footprint) -> Result<Vec<Vec<u8>>> {
    let mut graphics = definition
        .items
        .iter()
        .filter_map(|item| {
            if item.type_url.ends_with("BoardGraphicShape") {
                Some(
                    kiapi::board::types::BoardGraphicShape::decode(item.value.as_slice()).map(
                        |mut shape| {
                            shape.id = None;
                            shape.net = None;
                            shape.locked = kiapi::common::types::LockedState::LsUnlocked as i32;
                            if let Some(graphic) = shape.shape.as_mut() {
                                if let Some(stroke) = graphic
                                    .attributes
                                    .as_mut()
                                    .and_then(|attributes| attributes.stroke.as_mut())
                                {
                                    if matches!(
                                        stroke.style(),
                                        kiapi::common::types::StrokeLineStyle::SlsUnknown
                                            | kiapi::common::types::StrokeLineStyle::SlsDefault
                                            | kiapi::common::types::StrokeLineStyle::SlsSolid
                                    ) {
                                        stroke.style =
                                            kiapi::common::types::StrokeLineStyle::SlsSolid as i32;
                                    }
                                    stroke.color = None;
                                }
                                konnect_ipc::normalize_footprint_graphic_geometry(graphic);
                            }
                            shape.encode_to_vec()
                        },
                    ),
                )
            } else if item.type_url.ends_with("BoardText") {
                Some(
                    kiapi::board::types::BoardText::decode(item.value.as_slice()).map(
                        |mut text| {
                            text.id = None;
                            text.encode_to_vec()
                        },
                    ),
                )
            } else {
                None
            }
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    graphics.sort();
    Ok(graphics)
}

fn library_owned_attributes(
    attributes: Option<&kiapi::board::types::FootprintAttributes>,
) -> Option<(bool, bool, bool, bool, i32, bool)> {
    attributes.map(|attributes| {
        let mounting_style = match attributes.mounting_style() {
            kiapi::board::types::FootprintMountingStyle::FmsUnknown
            | kiapi::board::types::FootprintMountingStyle::FmsUnspecified => {
                kiapi::board::types::FootprintMountingStyle::FmsUnspecified as i32
            }
            style => style as i32,
        };
        (
            attributes.exclude_from_position_files,
            attributes.exclude_from_bill_of_materials,
            attributes.exempt_from_courtyard_requirement,
            attributes.allow_soldermask_bridges,
            mounting_style,
            attributes.not_in_schematic,
        )
    })
}

fn field_text(field: &Option<kiapi::board::types::Field>) -> String {
    field
        .as_ref()
        .and_then(|field| field.text.as_ref())
        .and_then(|text| text.text.as_ref())
        .map(|text| text.text.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use konnect_ipc::builders;
    use konnect_ipc::gen::kiapi;
    use prost::Message;
    use std::collections::{BTreeMap, BTreeSet};

    /// The serialization KiCad 10 actually writes — produced by running this
    /// module's hand-written fixture through `kicad-cli fp upgrade` — parses,
    /// and the two universal flags KiCad stamps into every saved footprint
    /// (`embedded_fonts no`, `duplicate_pad_numbers_are_jumpers no`, plus
    /// `remove_unused_layers no` on pads) do not refuse it. The hand-written
    /// fixture alone cannot catch this class: it shares this module's own
    /// assumptions about the format (see the fixtures-from-KiCad rule in
    /// CONTRIBUTING).
    #[test]
    fn a_footprint_kicad_saved_is_accepted_not_refused() {
        let source = KICAD_LIBRARY_FOOTPRINT;
        let library = parse_library_footprint("Konnect:Socket", source)
            .expect("KiCad's own serialization of the fixture must parse");
        assert_eq!(library.pads.len(), 4, "two '1' variants plus '2' and '3'");
        assert_eq!(
            library.graphics.len(),
            3,
            "one fp_line, one fp_poly, and KiCad's standard fab reference text"
        );
        assert!(library.graphics.iter().any(|graphic| {
            matches!(
                graphic,
                konnect_ipc::IpcGraphicDefinition::Text {
                    text,
                    position,
                    rotation,
                    layer,
                    size,
                    stroke_width_mm,
                } if text == "${REFERENCE}"
                    && *position == (0.0, 1.5)
                    && *rotation == 0.0
                    && layer == "F.Fab"
                    && *size == 1.0
                    && *stroke_width_mm == 0.15
            )
        }));
        assert_eq!(library.datasheet.as_deref(), Some("new-datasheet.pdf"));
        assert_eq!(
            library
                .properties
                .iter()
                .map(|property| (property.name.as_str(), field_text_value(property)))
                .collect::<Vec<_>>(),
            vec![
                ("KiLib_Generator", "konnect_test_generator"),
                ("AssemblyVendor", "Example Assembly"),
            ]
        );
        assert!(library.properties.iter().all(|property| !property.visible));
        assert_eq!(library.models.len(), 1);

        // The same flags at a non-default value carry semantics the typed
        // rebuild cannot represent, so they refuse rather than silently drop.
        for (from, to) in [
            ("(embedded_fonts no)", "(embedded_fonts yes)"),
            ("(remove_unused_layers no)", "(remove_unused_layers yes)"),
        ] {
            let flipped = source.replace(from, to);
            assert_ne!(flipped, source, "fixture must contain {from}");
            let error = parse_library_footprint("Konnect:Socket", &flipped)
                .expect_err("a non-default flag must refuse")
                .to_string();
            assert!(
                error.contains("not supported"),
                "refusal names the unsupported clause: {error}"
            );
        }
    }

    #[test]
    fn stock_kicad_generator_property_is_preserved_as_a_typed_field() {
        // Unmodified KiCad 10.0.5 standard-library output from
        // Capacitor_SMD.pretty/C_0603_1608Metric.kicad_mod. This is the stock
        // footprint that exposed #373; keeping its real generator-authored
        // shape prevents a synthetic fixture from agreeing with this parser.
        let source = include_str!("../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");
        let library = parse_library_footprint("Capacitor_SMD:C_0603_1608Metric", source)
            .expect("the stock KiCad generator footprint must parse losslessly");

        assert_eq!(library.pads.len(), 2);
        let generator = library
            .properties
            .iter()
            .find(|property| property.name == "KiLib_Generator")
            .expect("KiCad's generator property must remain a typed Field");
        assert_eq!(field_text_value(generator), "SMD_2terminal_chip_molded");
        assert!(!generator.visible);
        assert_eq!(library.models.len(), 1);
    }

    #[test]
    fn visible_property_is_a_field_only_and_clause_order_does_not_change_its_angle() {
        let source = KICAD_LIBRARY_FOOTPRINT.replace(
            "\t\t(at 0.5 0.75 15)\n\t\t(layer \"F.Fab\")\n\t\t(hide yes)",
            "\t\t(layer \"F.Fab\")\n\t\t(hide no)\n\t\t(at 0.5 0.75 15)",
        );
        assert_ne!(source, KICAD_LIBRARY_FOOTPRINT);

        let library = parse_library_footprint("Konnect:Socket", &source).unwrap();
        let property = library
            .properties
            .iter()
            .find(|property| property.name == "AssemblyVendor")
            .unwrap();
        assert!(property.visible);
        assert_eq!(
            property
                .text
                .as_ref()
                .unwrap()
                .text
                .as_ref()
                .unwrap()
                .attributes
                .as_ref()
                .unwrap()
                .angle
                .as_ref()
                .unwrap()
                .value_degrees,
            15.0
        );
        assert!(!library.graphics.iter().any(|graphic| matches!(
            graphic,
            konnect_ipc::IpcGraphicDefinition::Text { text, .. }
                if text == "Example Assembly"
        )));
    }

    /// `preserved` must be a comparison of the rebuilt instance against the
    /// board's, never a policy constant: when the two instances genuinely
    /// diverge, the flags have to say so.
    #[test]
    fn preserved_flags_come_from_comparison_not_policy() {
        let pad = |number: &str, net: Option<&str>| {
            let item = kiapi::board::types::Pad {
                number: number.to_string(),
                net: net.map(|name| kiapi::board::types::Net {
                    code: Some(kiapi::board::types::NetCode { value: 3 }),
                    name: name.to_string(),
                }),
                ..Default::default()
            };
            builders::pack_any(&item, "kiapi.board.types.Pad")
        };
        let current = kiapi::board::types::FootprintInstance {
            position: Some(builders::vec2(10.0, 20.0).unwrap()),
            orientation: Some(kiapi::common::types::Angle {
                value_degrees: 90.0,
            }),
            layer: kiapi::board::types::BoardLayer::BlFCu as i32,
            definition: Some(kiapi::board::types::Footprint {
                items: vec![pad("1", Some("GND"))],
                ..Default::default()
            }),
            ..Default::default()
        };
        let old_nets = BTreeMap::from([(
            "1".to_string(),
            kiapi::board::types::Net {
                code: Some(kiapi::board::types::NetCode { value: 3 }),
                name: "GND".to_string(),
            },
        )]);

        let identical = PreservedState::derive(&current, &current.clone(), &old_nets);
        assert!(
            identical.position
                && identical.rotation
                && identical.layer
                && identical.locked
                && identical.kiid
                && identical.symbol_path
                && identical.pad_nets
                && identical.instance_overrides,
            "an untouched instance preserves everything: {identical:?}"
        );

        let mut moved = current.clone();
        moved.position = Some(builders::vec2(99.0, 99.0).unwrap());
        moved.definition.as_mut().unwrap().items = vec![pad("1", None)];
        let diverged = PreservedState::derive(&current, &moved, &old_nets);
        assert!(
            !diverged.position,
            "a moved instance must report position unpreserved"
        );
        assert!(
            !diverged.pad_nets,
            "a dropped pad net must report pad_nets unpreserved"
        );
        assert!(
            diverged.rotation && diverged.layer && diverged.kiid,
            "untouched fields stay preserved: {diverged:?}"
        );
    }

    #[test]
    fn authored_mandatory_library_fields_are_retained() {
        let library = parse_library_footprint("Test:Socket", KICAD_LIBRARY_FOOTPRINT).unwrap();
        let reference = library
            .definition
            .reference_field
            .as_ref()
            .expect("library Reference field");
        let attributes = reference
            .text
            .as_ref()
            .unwrap()
            .text
            .as_ref()
            .unwrap()
            .attributes
            .as_ref()
            .unwrap();
        assert!(attributes.stroke_width.as_ref().unwrap().value_nm > 0);
        assert_eq!(attributes.line_spacing, 1.0);
        assert!(attributes.keep_upright);
    }

    #[test]
    fn refresh_fields_refuse_nonfinite_rotation_before_preparing_a_write() {
        use kiapi::board::types::BoardLayer;

        for (layer, local_angle, is_back, expected_angle) in [
            (BoardLayer::BlFCu, f64::MAX, false, 54.0),
            (BoardLayer::BlBCu, -f64::MAX, true, 20.0),
        ] {
            let mut property = field("LCSC", "C25804", 1.0, 2.0, true);
            let board_text = property.text.as_mut().unwrap();
            board_text.layer = BoardLayer::BlFFab as i32;
            let attributes = board_text
                .text
                .as_mut()
                .unwrap()
                .attributes
                .as_mut()
                .unwrap();
            attributes.stroke_width = Some(builders::distance(0.15).unwrap());
            attributes.line_spacing = 1.0;
            attributes.keep_upright = true;
            attributes.angle.as_mut().unwrap().value_degrees = local_angle;
            let before_property = property.clone();
            let mut current = current_instance(layer);
            current.orientation.as_mut().unwrap().value_degrees = f64::MAX / 4.0;
            let before_current = current.clone();
            let mut library =
                parse_library_footprint("Test:Socket", KICAD_LIBRARY_FOOTPRINT).unwrap();
            library.properties = vec![property.clone()];
            let result = build_updated_instance(
                &current,
                &library,
                &BTreeMap::new(),
                &BTreeSet::new(),
                false,
                2,
            );
            let error = result.expect_err("nonfinite field rotation prepared a write");
            let message = format!("{error:#}");
            assert!(
                message.contains("LCSC")
                    && message.contains("rotation")
                    && message.contains("finite"),
                "{message}"
            );
            assert_eq!(current, before_current);
            assert_eq!(property, before_property);

            property
                .text
                .as_mut()
                .unwrap()
                .text
                .as_mut()
                .unwrap()
                .attributes
                .as_mut()
                .unwrap()
                .angle
                .as_mut()
                .unwrap()
                .value_degrees = 17.0;
            let transformed = transform_library_property(
                &property,
                &builders::vec2(10.0, 20.0).unwrap(),
                37.0,
                is_back,
                2,
            )
            .unwrap();
            let text = transformed.text.unwrap().text.unwrap();
            assert_eq!(text.text, "C25804");
            assert_eq!(
                text.attributes.unwrap().angle.unwrap().value_degrees,
                expected_angle
            );
        }
    }

    #[test]
    fn issue26_custom_field_mirroring_uses_the_live_stack_and_side_mask() {
        use kiapi::board::types::BoardLayer;
        for (layer, expected_layer, mirrored) in [
            (BoardLayer::BlIn1Cu, BoardLayer::BlIn2Cu, true),
            (BoardLayer::BlDwgsUser, BoardLayer::BlDwgsUser, false),
        ] {
            let mut property = field("Assembly", "Custom", 1.0, 2.0, true);
            let text = property.text.as_mut().unwrap();
            text.layer = layer as i32;
            text.text
                .as_mut()
                .unwrap()
                .attributes
                .as_mut()
                .unwrap()
                .mirrored = false;
            let transformed = transform_library_property(
                &property,
                &builders::vec2(10.0, 20.0).unwrap(),
                0.0,
                true,
                4,
            )
            .unwrap();
            let text = transformed.text.unwrap();
            assert_eq!(text.layer, expected_layer as i32);
            let text = text.text.unwrap();
            assert_eq!(text.position, Some(builders::vec2(11.0, 18.0).unwrap()));
            assert_eq!(text.attributes.unwrap().mirrored, mirrored);
        }
        let mut unsupported = field("Assembly", "Custom", 1.0, 2.0, true);
        unsupported.text.as_mut().unwrap().layer = BoardLayer::BlIn3Cu as i32;
        assert!(transform_library_property(
            &unsupported,
            &builders::vec2(10.0, 20.0).unwrap(),
            0.0,
            true,
            4,
        )
        .is_err());
    }

    #[test]
    fn issue26_reset_text_restores_effects_retains_values_and_converges() {
        let library = parse_library_footprint("Test:Socket", KICAD_LIBRARY_FOOTPRINT).unwrap();
        let current = current_instance(kiapi::board::types::BoardLayer::BlFCu);
        let first = build_updated_instance(
            &current,
            &library,
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            2,
        )
        .unwrap();
        let mut applied =
            kiapi::board::types::FootprintInstance::decode(first.item.value.as_slice()).unwrap();
        applied
            .reference_field
            .as_mut()
            .unwrap()
            .text
            .as_mut()
            .unwrap()
            .text
            .as_mut()
            .unwrap()
            .attributes
            .as_mut()
            .unwrap()
            .size = Some(builders::vec2(2.0, 2.0).unwrap());
        let preserved = build_updated_instance(
            &applied,
            &library,
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            2,
        )
        .unwrap();
        assert!(!preserved.changed_domains.contains(&ChangedDomain::Text));
        let reset = build_updated_instance(
            &applied,
            &library,
            &BTreeMap::new(),
            &BTreeSet::new(),
            true,
            2,
        )
        .unwrap();
        assert!(reset.changed_domains.contains(&ChangedDomain::Text));
        let reset =
            kiapi::board::types::FootprintInstance::decode(reset.item.value.as_slice()).unwrap();
        assert_eq!(field_text(&reset.reference_field), "SW1");
        assert_eq!(field_text(&reset.value_field), "Socket Value");
        let repeated = build_updated_instance(
            &reset,
            &library,
            &BTreeMap::new(),
            &BTreeSet::new(),
            true,
            2,
        )
        .unwrap();
        assert!(
            repeated.changed_domains.is_empty(),
            "{:?}",
            repeated.changed_domains
        );
    }

    #[test]
    fn issue26_reset_text_is_typed_and_changes_reviewed_revision() {
        assert!(parse_filters(&json!({"reset_text":"true"})).is_err());
        assert!(!parse_filters(&json!({})).unwrap().reset_text);
        let (_temp, board, items) = plan_fixture();
        let initial = plan_updates(
            &board,
            &items,
            &BTreeMap::new(),
            &BTreeSet::new(),
            &parse_filters(&json!({})).unwrap(),
            2,
        );
        let reset = plan_updates(
            &board,
            &items,
            &BTreeMap::new(),
            &BTreeSet::new(),
            &parse_filters(&json!({"reset_text":true})).unwrap(),
            2,
        );
        assert_ne!(initial.plan_revision, reset.plan_revision);
    }

    #[test]
    fn issue26_dashed_courtyard_survives_refresh_and_then_is_noop() {
        let source = KICAD_LIBRARY_FOOTPRINT.replace("(type solid)", "(type dash)");
        let library = parse_library_footprint("Test:Socket", &source).unwrap();
        let current = current_instance(kiapi::board::types::BoardLayer::BlFCu);
        let first = build_updated_instance(
            &current,
            &library,
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            2,
        )
        .unwrap();
        let applied =
            kiapi::board::types::FootprintInstance::decode(first.item.value.as_slice()).unwrap();
        let styles = applied
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|item| builders::any_is(item, "kiapi.board.types.BoardGraphicShape"))
            .map(|item| {
                kiapi::board::types::BoardGraphicShape::decode(item.value.as_slice())
                    .unwrap()
                    .shape
                    .unwrap()
                    .attributes
                    .unwrap()
                    .stroke
                    .unwrap()
                    .style
            })
            .collect::<Vec<_>>();
        assert!(!styles.is_empty());
        assert!(styles
            .iter()
            .all(|style| *style == kiapi::common::types::StrokeLineStyle::SlsDash as i32));
        let repeated = build_updated_instance(
            &applied,
            &library,
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            2,
        )
        .unwrap();
        assert!(
            repeated.changed_domains.is_empty(),
            "{:?}",
            repeated.changed_domains
        );
    }

    const KICAD_LIBRARY_FOOTPRINT: &str =
        include_str!("../../tests/fixtures/socket_kicad10.kicad_mod");

    const LIBRARY_FOOTPRINT: &str = r#"
(footprint "Socket"
  (version 20240108)
  (generator "konnect")
  (layer "F.Cu")
  (descr "updated description")
  (tags "keyboard socket")
  (attr smd exclude_from_pos_files)
  (fp_line (start -1 -2) (end 3 4)
    (stroke (width 0.12) (type solid))
    (layer "F.SilkS"))
  (fp_poly (pts (xy -2 -1) (xy 2 -1) (xy 2 1) (xy -2 1))
    (stroke (width 0.05) (type solid))
    (fill none)
    (layer "B.CrtYd"))
  (pad "1" smd roundrect (at -2 0 15) (size 2 1)
    (layers "B.Cu" "B.Paste" "B.Mask")
    (roundrect_rratio 0.2))
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "B.Cu"))
  (pad "2" thru_hole circle (at 2 0) (size 3 3)
    (layers "*.Cu" "*.Mask") (drill 1))
  (pad "3" smd rect (at 0 3) (size 1 1) (layers "F.Cu"))
  (fp_text reference "REF**" (at 0 -4 0) (layer "F.SilkS")
    (effects (font (size 1 1) (thickness 0.15))))
  (fp_text value "Socket" (at 0 4 0) (layer "F.Fab")
    (effects (font (size 1 1) (thickness 0.15))))
  (fp_text user "${REFERENCE}" (at 0 1.5 0) (layer "F.Fab")
    (uuid "f96c2efe-5925-4f74-81d2-f89a56f57e13")
    (effects (font (size 0.8 0.8) (thickness 0.11))))
  (property "Datasheet" "new-datasheet.pdf" (at 0 0) (layer "F.Fab") (hide yes))
  (property "Description" "new field description" (at 0 0) (layer "F.Fab") (hide yes))
  (model "../models/Socket.step"
    (offset (xyz 1 2 3))
    (scale (xyz -1 1 1))
    (rotate (xyz 90 0 45)))
)
"#;

    fn field(name: &str, value: &str, x: f64, y: f64, visible: bool) -> kiapi::board::types::Field {
        kiapi::board::types::Field {
            name: name.to_string(),
            visible,
            text: Some(kiapi::board::types::BoardText {
                text: Some(kiapi::common::types::Text {
                    position: Some(builders::vec2(x, y).unwrap()),
                    attributes: Some(kiapi::common::types::TextAttributes {
                        size: Some(builders::vec2(1.25, 1.25).unwrap()),
                        angle: Some(kiapi::common::types::Angle {
                            value_degrees: 17.0,
                        }),
                        mirrored: true,
                        ..Default::default()
                    }),
                    text: value.to_string(),
                    ..Default::default()
                }),
                layer: kiapi::board::types::BoardLayer::BlBSilkS as i32,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn current_pad(number: &str, net_name: &str, net_code: i32) -> prost_types::Any {
        builders::pack_any(
            &kiapi::board::types::Pad {
                id: Some(kiapi::common::types::Kiid {
                    value: format!("old-pad-{number}-{net_code}"),
                }),
                number: number.to_string(),
                net: Some(kiapi::board::types::Net {
                    code: Some(kiapi::board::types::NetCode { value: net_code }),
                    name: net_name.to_string(),
                }),
                position: Some(builders::vec2(100.0, 50.0).unwrap()),
                ..Default::default()
            },
            "kiapi.board.types.Pad",
        )
    }

    fn current_instance(
        layer: kiapi::board::types::BoardLayer,
    ) -> kiapi::board::types::FootprintInstance {
        let reference = field("Reference", "SW1", 101.0, 48.0, true);
        let value = field("Value", "Socket Value", 99.0, 52.0, false);
        kiapi::board::types::FootprintInstance {
            id: Some(kiapi::common::types::Kiid {
                value: "instance-kiid".to_string(),
            }),
            position: Some(builders::vec2(100.0, 50.0).unwrap()),
            orientation: Some(kiapi::common::types::Angle {
                value_degrees: 37.0,
            }),
            layer: layer as i32,
            locked: kiapi::common::types::LockedState::LsLocked as i32,
            definition: Some(kiapi::board::types::Footprint {
                id: Some(kiapi::common::types::LibraryIdentifier {
                    library_nickname: "Test".to_string(),
                    entry_name: "Socket".to_string(),
                }),
                reference_field: Some(reference.clone()),
                value_field: Some(value.clone()),
                items: vec![current_pad("1", "ROW1", 11), current_pad("2", "COL1", 12)],
                ..Default::default()
            }),
            reference_field: Some(reference),
            value_field: Some(value),
            datasheet_field: Some(field("Datasheet", "placed-datasheet", 0.0, 0.0, false)),
            description_field: Some(field("Description", "placed-description", 0.0, 0.0, false)),
            attributes: Some(kiapi::board::types::FootprintAttributes {
                not_in_schematic: false,
                exclude_from_bill_of_materials: true,
                do_not_populate: true,
                ..Default::default()
            }),
            overrides: Some(kiapi::board::types::FootprintDesignRuleOverrides {
                copper_clearance: Some(builders::distance(0.3).unwrap()),
                ..Default::default()
            }),
            symbol_path: Some(kiapi::common::types::SheetPath {
                path: vec![kiapi::common::types::Kiid {
                    value: "sheet-kiid".to_string(),
                }],
                path_human_readable: "/Keyboard".to_string(),
            }),
            symbol_sheet_name: "Keyboard".to_string(),
            symbol_sheet_filename: "keyboard.kicad_sch".to_string(),
            symbol_footprint_filters: "Test:*".to_string(),
            parent: None,
        }
    }

    fn decoded_pads(
        instance: &kiapi::board::types::FootprintInstance,
    ) -> Vec<kiapi::board::types::Pad> {
        instance
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|item| item.type_url.ends_with("kiapi.board.types.Pad"))
            .map(|item| kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap())
            .collect()
    }

    fn decoded_custom_fields(
        instance: &kiapi::board::types::FootprintInstance,
    ) -> Vec<kiapi::board::types::Field> {
        instance
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|item| item.type_url.ends_with("kiapi.board.types.Field"))
            .map(|item| {
                kiapi::board::types::Field::decode(item.value.as_slice())
                    .expect("custom field must decode")
            })
            .collect()
    }

    fn field_text_value(field: &kiapi::board::types::Field) -> &str {
        field
            .text
            .as_ref()
            .and_then(|text| text.text.as_ref())
            .map(|text| text.text.as_str())
            .unwrap_or_default()
    }

    #[test]
    fn parses_supported_library_definition_without_dropping_domains() {
        let library = parse_library_footprint("Test:Socket", LIBRARY_FOOTPRINT).unwrap();

        assert_eq!(library.library_id, "Test:Socket");
        assert_eq!(
            library.definition.attributes.as_ref().unwrap().description,
            "updated description"
        );
        assert_eq!(
            library.definition.attributes.as_ref().unwrap().keywords,
            "keyboard socket"
        );
        assert_eq!(library.pads.len(), 4);
        assert_eq!(library.graphics.len(), 3);
        assert!(library.graphics.iter().any(|graphic| matches!(
            graphic,
            konnect_ipc::IpcGraphicDefinition::Text {
                text,
                size,
                stroke_width_mm,
                ..
            } if text == "${REFERENCE}" && *size == 0.8 && *stroke_width_mm == 0.11
        )));
        assert_eq!(library.models.len(), 1);
        let model = &library.models[0];
        assert_eq!(model.filename, "../models/Socket.step");
        assert_eq!(model.offset.as_ref().unwrap().x_nm, 1.0);
        assert_eq!(model.scale.as_ref().unwrap().x_nm, -1.0);
        assert_eq!(model.rotation.as_ref().unwrap().z_nm, 45.0);
    }

    #[test]
    fn refresh_inner_copper_uses_the_eight_layer_stack() {
        let library = parse_library_footprint("Test:Socket", LIBRARY_FOOTPRINT).unwrap();
        let mut pad = library.pads[0].clone();
        pad.layers = vec!["In2.Cu".to_string()];
        let mirrored = mirror_pad(&pad, 8).unwrap();
        assert_eq!(mirrored.layers, ["In5.Cu"]);
    }

    #[test]
    fn refresh_rebuild_mirrors_inner_pad_graphic_and_numeric_field_layers() {
        let current = current_instance(kiapi::board::types::BoardLayer::BlBCu);
        let mut library = parse_library_footprint("Test:Socket", KICAD_LIBRARY_FOOTPRINT).unwrap();
        library.pads[0].layers = vec!["In2.Cu".to_string()];
        library
            .graphics
            .push(konnect_ipc::IpcGraphicDefinition::Line {
                start: (1.0, 2.0),
                end: (3.0, 4.0),
                layer: "In2.Cu".to_string(),
                width: 0.1,
                style: kiapi::common::types::StrokeLineStyle::SlsSolid,
            });
        library
            .graphics
            .push(konnect_ipc::IpcGraphicDefinition::Text {
                text: "inner text".to_string(),
                position: (1.0, 2.0),
                rotation: 0.0,
                layer: "In2.Cu".to_string(),
                size: 1.0,
                stroke_width_mm: 0.15,
            });
        let field = library
            .properties
            .first_mut()
            .expect("fixture contains a custom property");
        field.text.as_mut().unwrap().layer = kiapi::board::types::BoardLayer::BlIn2Cu as i32;
        let field_name = field.name.clone();
        let prepared = build_updated_instance(
            &current,
            &library,
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            8,
        )
        .unwrap();
        let updated =
            kiapi::board::types::FootprintInstance::decode(prepared.item.value.as_slice()).unwrap();
        let definition = updated.definition.as_ref().unwrap();
        let pads = definition
            .items
            .iter()
            .filter(|item| item.type_url.ends_with(".Pad"))
            .map(|item| kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap())
            .collect::<Vec<_>>();
        assert!(pads[0]
            .pad_stack
            .as_ref()
            .unwrap()
            .layers
            .contains(&(kiapi::board::types::BoardLayer::BlIn5Cu as i32)));
        let fields = decoded_custom_fields(&updated);
        assert_eq!(
            fields
                .iter()
                .find(|field| field.name == field_name)
                .unwrap()
                .text
                .as_ref()
                .unwrap()
                .layer,
            kiapi::board::types::BoardLayer::BlIn5Cu as i32
        );
        let text = definition
            .items
            .iter()
            .filter(|item| konnect_ipc::builders::any_is(item, "kiapi.board.types.BoardText"))
            .map(|item| kiapi::board::types::BoardText::decode(item.value.as_slice()).unwrap())
            .find(|item| {
                item.text
                    .as_ref()
                    .is_some_and(|text| text.text == "inner text")
            })
            .unwrap();
        assert_eq!(text.layer, kiapi::board::types::BoardLayer::BlIn5Cu as i32);
        assert!(text.text.unwrap().attributes.unwrap().mirrored);
        let graphic = mirror_graphic(&library.graphics[library.graphics.len() - 2], 8).unwrap();
        assert!(
            matches!(graphic, konnect_ipc::IpcGraphicDefinition::Line { layer, .. } if layer == "In5.Cu")
        );
        assert!(build_updated_instance(
            &current,
            &library,
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            2
        )
        .is_err());
    }

    fn instance_for_layer_readback() -> kiapi::board::types::FootprintInstance {
        let current = current_instance(kiapi::board::types::BoardLayer::BlBCu);
        let library = parse_library_footprint("Test:Socket", KICAD_LIBRARY_FOOTPRINT).unwrap();
        let prepared = build_updated_instance(
            &current,
            &library,
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            8,
        )
        .unwrap();
        kiapi::board::types::FootprintInstance::decode(prepared.item.value.as_slice()).unwrap()
    }

    #[test]
    fn layer_readback_refuses_missing_padstack_evidence() {
        let incomplete = current_instance(kiapi::board::types::BoardLayer::BlBCu);
        let error = footprint_layer_states(&[builders::pack_any(
            &incomplete,
            "kiapi.board.types.FootprintInstance",
        )])
        .unwrap_err();
        assert!(error.to_string().contains("pad has no padstack"));
    }

    #[test]
    fn layer_readback_ignores_generated_ids_and_item_order_but_preserves_multiplicity() {
        let expected = instance_for_layer_readback();
        let packed = builders::pack_any(&expected, "kiapi.board.types.FootprintInstance");
        let signature = footprint_layer_states(&[packed]).unwrap();
        let mut observed = expected.clone();
        let definition = observed.definition.as_mut().unwrap();
        definition.items.reverse();
        for item in &mut definition.items {
            if builders::any_is(item, "kiapi.board.types.Pad") {
                let mut pad = kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap();
                pad.id = Some(kiapi::common::types::Kiid {
                    value: "assigned-by-kicad".into(),
                });
                *item = builders::pack_any(&pad, "kiapi.board.types.Pad");
            }
        }
        verify_footprint_layer_readback(
            &signature,
            &[builders::pack_any(
                &observed,
                "kiapi.board.types.FootprintInstance",
            )],
        )
        .unwrap();
        let duplicate = expected
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .find(|item| builders::any_is(item, "kiapi.board.types.Pad"))
            .unwrap()
            .clone();
        observed.definition.as_mut().unwrap().items.push(duplicate);
        let duplicate_signature = footprint_layer_states(&[builders::pack_any(
            &observed,
            "kiapi.board.types.FootprintInstance",
        )])
        .unwrap();
        assert!(verify_footprint_layer_readback(
            &duplicate_signature,
            &[builders::pack_any(
                &expected,
                "kiapi.board.types.FootprintInstance"
            )]
        )
        .is_err());
    }

    #[test]
    fn layer_readback_rejects_lost_inner_pad_layer_and_duplicate_footprints() {
        let mut expected = instance_for_layer_readback();
        let pad = expected
            .definition
            .as_mut()
            .unwrap()
            .items
            .iter_mut()
            .find(|item| builders::any_is(item, "kiapi.board.types.Pad"))
            .unwrap();
        let mut inner_pad = kiapi::board::types::Pad::decode(pad.value.as_slice()).unwrap();
        inner_pad.pad_stack.as_mut().unwrap().layers =
            vec![kiapi::board::types::BoardLayer::BlIn2Cu as i32];
        *pad = builders::pack_any(&inner_pad, "kiapi.board.types.Pad");
        let packed = builders::pack_any(&expected, "kiapi.board.types.FootprintInstance");
        let signature = footprint_layer_states(std::slice::from_ref(&packed)).unwrap();
        assert!(verify_footprint_layer_readback(&signature, &[packed.clone(), packed]).is_err());
        let mut observed = expected.clone();
        let pad = observed
            .definition
            .as_mut()
            .unwrap()
            .items
            .iter_mut()
            .find(|item| builders::any_is(item, "kiapi.board.types.Pad"))
            .unwrap();
        let mut changed = kiapi::board::types::Pad::decode(pad.value.as_slice()).unwrap();
        changed.pad_stack.as_mut().unwrap().layers =
            vec![kiapi::board::types::BoardLayer::BlIn5Cu as i32];
        *pad = builders::pack_any(&changed, "kiapi.board.types.Pad");
        assert!(verify_footprint_layer_readback(
            &signature,
            &[builders::pack_any(
                &observed,
                "kiapi.board.types.FootprintInstance"
            )]
        )
        .is_err());
    }

    #[test]
    fn layer_readback_rejects_swapped_layers_on_same_number_pads() {
        for same_position in [false, true] {
            let mut expected = instance_for_layer_readback();
            let pads = expected
                .definition
                .as_mut()
                .unwrap()
                .items
                .iter_mut()
                .filter(|item| builders::any_is(item, "kiapi.board.types.Pad"))
                .take(2)
                .collect::<Vec<_>>();
            assert_eq!(pads.len(), 2);
            for (index, item) in pads.into_iter().enumerate() {
                let mut pad = kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap();
                pad.number = "1".into();
                pad.position = Some(kiapi::common::types::Vector2 {
                    x_nm: if same_position { 0 } else { index as i64 * 100 },
                    y_nm: 0,
                });
                let stack = pad.pad_stack.as_mut().unwrap();
                stack.layers = vec![if index == 0 {
                    kiapi::board::types::BoardLayer::BlIn2Cu as i32
                } else {
                    kiapi::board::types::BoardLayer::BlIn5Cu as i32
                }];
                stack.angle = Some(kiapi::common::types::Angle {
                    value_degrees: index as f64 * 90.0,
                });
                stack.copper_layers[0].size = Some(kiapi::common::types::Vector2 {
                    x_nm: 100 + index as i64 * 100,
                    y_nm: 100,
                });
                *item = builders::pack_any(&pad, "kiapi.board.types.Pad");
            }
            let signature = footprint_layer_states(&[builders::pack_any(
                &expected,
                "kiapi.board.types.FootprintInstance",
            )])
            .unwrap();
            let mut observed = expected;
            for item in observed
                .definition
                .as_mut()
                .unwrap()
                .items
                .iter_mut()
                .filter(|item| builders::any_is(item, "kiapi.board.types.Pad"))
                .take(2)
            {
                let mut pad = kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap();
                let layer = &mut pad.pad_stack.as_mut().unwrap().layers[0];
                *layer = if *layer == kiapi::board::types::BoardLayer::BlIn2Cu as i32 {
                    kiapi::board::types::BoardLayer::BlIn5Cu as i32
                } else {
                    kiapi::board::types::BoardLayer::BlIn2Cu as i32
                };
                *item = builders::pack_any(&pad, "kiapi.board.types.Pad");
            }
            assert!(verify_footprint_layer_readback(
                &signature,
                &[builders::pack_any(
                    &observed,
                    "kiapi.board.types.FootprintInstance"
                )]
            )
            .is_err());
        }
    }

    fn layer_readback_instance(
        items: Vec<prost_types::Any>,
    ) -> kiapi::board::types::FootprintInstance {
        kiapi::board::types::FootprintInstance {
            id: Some(kiapi::common::types::Kiid {
                value: "geometry-footprint".into(),
            }),
            layer: kiapi::board::types::BoardLayer::BlFCu as i32,
            definition: Some(kiapi::board::types::Footprint {
                items,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn native_pad_normalization_ignores_inactive_corner_defaults() {
        use kiapi::board::types::PadStackShape;

        let original = kiapi::board::types::FootprintInstance::decode(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/issue_474_r1.ipc.bin"
            ))
            .as_slice(),
        )
        .unwrap();
        let mut observed = original.clone();
        let mut captured_unused_chamfer = false;
        for item in &mut observed.definition.as_mut().unwrap().items {
            if !builders::any_is(item, "kiapi.board.types.Pad") {
                continue;
            }
            let mut pad = kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap();
            for layer in &mut pad.pad_stack.as_mut().unwrap().copper_layers {
                if layer.shape != PadStackShape::PssChamferedrect as i32 {
                    captured_unused_chamfer |= layer.chamfer_ratio != 0.0;
                    layer.chamfer_ratio = 0.0;
                    layer.chamfered_corners = Some(kiapi::board::types::ChamferedRectCorners {
                        top_left: true,
                        ..Default::default()
                    });
                }
                if !matches!(
                    layer.shape(),
                    PadStackShape::PssRoundrect | PadStackShape::PssChamferedrect
                ) {
                    layer.corner_rounding_ratio = 0.4;
                }
            }
            *item = builders::pack_any(&pad, "kiapi.board.types.Pad");
        }
        assert!(captured_unused_chamfer);
        assert_ne!(original.encode_to_vec(), observed.encode_to_vec());
        assert_eq!(
            normalized_items(original.definition.as_ref().unwrap(), "Pad").unwrap(),
            normalized_items(observed.definition.as_ref().unwrap(), "Pad").unwrap()
        );
        assert_eq!(
            footprint_layer_state(&original).unwrap(),
            footprint_layer_state(&observed).unwrap()
        );
    }

    #[test]
    fn native_pad_normalization_retains_active_corner_geometry() {
        use kiapi::board::types::PadStackShape;

        for (shape, change) in [
            (PadStackShape::PssRoundrect, "rounding"),
            (PadStackShape::PssChamferedrect, "rounding"),
            (PadStackShape::PssChamferedrect, "chamfer"),
            (PadStackShape::PssChamferedrect, "corners"),
        ] {
            let pad = kiapi::board::types::Pad {
                number: "1".into(),
                position: Some(Default::default()),
                pad_stack: Some(kiapi::board::types::PadStack {
                    copper_layers: vec![kiapi::board::types::PadStackLayer {
                        shape: shape as i32,
                        corner_rounding_ratio: 0.25,
                        chamfer_ratio: 0.2,
                        chamfered_corners: Some(kiapi::board::types::ChamferedRectCorners {
                            top_left: true,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            };
            let mut observed = pad.clone();
            let layer = &mut observed.pad_stack.as_mut().unwrap().copper_layers[0];
            match change {
                "rounding" => layer.corner_rounding_ratio = 0.1,
                "chamfer" => layer.chamfer_ratio = 0.3,
                "corners" => {
                    layer.chamfered_corners.as_mut().unwrap().top_left = false;
                    layer.chamfered_corners.as_mut().unwrap().top_right = true;
                }
                _ => unreachable!(),
            }
            assert_ne!(
                pad_layer_state(pad).unwrap(),
                pad_layer_state(observed).unwrap(),
                "{shape:?} {change}"
            );
        }
    }

    #[test]
    fn layer_readback_binds_layers_to_coincident_pad_rounding() {
        let make = |swapped: bool| {
            let items = [0.1, 0.25]
                .into_iter()
                .enumerate()
                .map(|(index, ratio)| {
                    let layer = if (index == 0) != swapped {
                        kiapi::board::types::BoardLayer::BlIn2Cu
                    } else {
                        kiapi::board::types::BoardLayer::BlIn5Cu
                    } as i32;
                    builders::pack_any(
                        &kiapi::board::types::Pad {
                            number: "1".into(),
                            position: Some(Default::default()),
                            r#type: kiapi::board::types::PadType::PtSmd as i32,
                            pad_stack: Some(kiapi::board::types::PadStack {
                                layers: vec![layer],
                                copper_layers: vec![kiapi::board::types::PadStackLayer {
                                    layer,
                                    shape: kiapi::board::types::PadStackShape::PssRoundrect as i32,
                                    size: Some(kiapi::common::types::Vector2 {
                                        x_nm: 1_000_000,
                                        y_nm: 1_000_000,
                                    }),
                                    corner_rounding_ratio: ratio,
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
            builders::pack_any(
                &layer_readback_instance(items),
                "kiapi.board.types.FootprintInstance",
            )
        };
        let expected = footprint_layer_states(&[make(false)]).unwrap();
        assert!(verify_footprint_layer_readback(&expected, &[make(false)]).is_ok());
        assert!(verify_footprint_layer_readback(&expected, &[make(true)]).is_err());
    }

    #[test]
    fn layer_readback_binds_layers_to_complete_text_geometry() {
        for difference in 0..9 {
            let make = |swapped: bool| {
                let items = (0..2)
                    .map(|index| {
                        let layer = if (index == 0) != swapped {
                            kiapi::board::types::BoardLayer::BlIn2Cu
                        } else {
                            kiapi::board::types::BoardLayer::BlIn5Cu
                        } as i32;
                        let mut attributes = kiapi::common::types::TextAttributes {
                            size: Some(kiapi::common::types::Vector2 {
                                x_nm: 1_000_000,
                                y_nm: 1_000_000,
                            }),
                            line_spacing: 1.0,
                            mirrored: true,
                            ..Default::default()
                        };
                        if index == 1 {
                            match difference {
                                0 => {
                                    attributes.angle = Some(kiapi::common::types::Angle {
                                        value_degrees: 90.0,
                                    })
                                }
                                1 => attributes.size.as_mut().unwrap().x_nm = 2_000_000,
                                2 => attributes.font_name = "KiCad Font".into(),
                                3 => {
                                    attributes.stroke_width =
                                        Some(kiapi::common::types::Distance { value_nm: 100_000 })
                                }
                                4 => attributes.bold = true,
                                5 => attributes.italic = true,
                                6 => attributes.underlined = true,
                                7 => attributes.keep_upright = true,
                                _ => {
                                    attributes.horizontal_alignment =
                                        kiapi::common::types::HorizontalAlignment::HaLeft as i32
                                }
                            }
                        }
                        builders::pack_any(
                            &kiapi::board::types::BoardText {
                                layer,
                                text: Some(kiapi::common::types::Text {
                                    text: "same anchor".into(),
                                    position: Some(Default::default()),
                                    attributes: Some(attributes),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                            "kiapi.board.types.BoardText",
                        )
                    })
                    .collect();
                builders::pack_any(
                    &layer_readback_instance(items),
                    "kiapi.board.types.FootprintInstance",
                )
            };
            let expected = footprint_layer_states(&[make(false)]).unwrap();
            assert!(verify_footprint_layer_readback(&expected, &[make(false)]).is_ok());
            assert!(
                verify_footprint_layer_readback(&expected, &[make(true)]).is_err(),
                "difference {difference}"
            );
        }
    }

    #[test]
    fn layer_readback_binds_layers_to_graphic_stroke_geometry() {
        for change_style in [false, true] {
            let make = |swapped: bool| {
                let items = (0..2)
                    .map(|index| {
                        let layer = if (index == 0) != swapped {
                            "In2.Cu"
                        } else {
                            "In5.Cu"
                        };
                        let mut graphic = builders::board_segment(
                            layer,
                            if index == 1 && !change_style {
                                0.2
                            } else {
                                0.1
                            },
                            0.0,
                            0.0,
                            1.0,
                            0.0,
                        )
                        .unwrap();
                        if index == 1 && change_style {
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
                                .style = kiapi::common::types::StrokeLineStyle::SlsDash as i32;
                        }
                        builders::pack_any(&graphic, "kiapi.board.types.BoardGraphicShape")
                    })
                    .collect();
                builders::pack_any(
                    &layer_readback_instance(items),
                    "kiapi.board.types.FootprintInstance",
                )
            };
            let expected = footprint_layer_states(&[make(false)]).unwrap();
            assert!(verify_footprint_layer_readback(&expected, &[make(false)]).is_ok());
            assert!(verify_footprint_layer_readback(&expected, &[make(true)]).is_err());
        }
    }

    #[test]
    fn layer_readback_accepts_native_arc_winding_but_checks_geometry() {
        let make = |start: (f64, f64), mid: (f64, f64), end: (f64, f64)| {
            layer_readback_instance(vec![builders::pack_any(
                &builders::board_arc(
                    "F.SilkS", 0.12, start.0, start.1, mid.0, mid.1, end.0, end.1,
                )
                .unwrap(),
                "kiapi.board.types.BoardGraphicShape",
            )])
        };
        let expected = footprint_layer_state(&make((1.0, 0.0), (0.0, -1.0), (-1.0, 0.0))).unwrap();
        assert_eq!(
            expected,
            footprint_layer_state(&make((-1.0, 0.0), (0.0, -1.0), (1.0, 0.0))).unwrap()
        );
        for (start, mid, end) in [
            ((-1.0, 0.0), (0.0, 1.0), (1.0, 0.0)),
            ((-2.0, 0.0), (0.0, -1.0), (1.0, 0.0)),
            ((-1.0, 0.0), (0.0, -1.0), (2.0, 0.0)),
        ] {
            assert_ne!(
                expected,
                footprint_layer_state(&make(start, mid, end)).unwrap()
            );
        }
    }

    #[test]
    fn library_refresh_revision_includes_live_copper_count() {
        let (_temp, board, items) = plan_fixture();
        let filters = UpdateFilters::default();
        let first = plan_updates(
            &board,
            &items,
            &BTreeMap::new(),
            &BTreeSet::new(),
            &filters,
            4,
        );
        let second = plan_updates(
            &board,
            &items,
            &BTreeMap::new(),
            &BTreeSet::new(),
            &filters,
            8,
        );
        assert_ne!(first.plan_revision, second.plan_revision);
    }

    #[test]
    fn merge_preserves_instance_state_and_nets_by_logical_pad_number() {
        let current = current_instance(kiapi::board::types::BoardLayer::BlBCu);
        let library = parse_library_footprint("Test:Socket", KICAD_LIBRARY_FOOTPRINT).unwrap();
        let prepared = build_updated_instance(
            &current,
            &library,
            &BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]),
            &BTreeSet::from(["ROW1".to_string(), "COL1".to_string()]),
            false,
            2,
        )
        .unwrap();
        let updated =
            kiapi::board::types::FootprintInstance::decode(prepared.item.value.as_slice()).unwrap();

        assert_eq!(updated.id, current.id);
        assert_eq!(updated.position, current.position);
        assert_eq!(updated.orientation, current.orientation);
        assert_eq!(updated.layer, current.layer);
        assert_eq!(updated.locked, current.locked);
        assert_eq!(updated.reference_field, current.reference_field);
        assert_eq!(updated.value_field, current.value_field);
        assert_eq!(field_text(&updated.datasheet_field), "new-datasheet.pdf");
        assert_eq!(
            updated
                .datasheet_field
                .as_ref()
                .and_then(|field| field.text.as_ref())
                .and_then(|text| text.text.as_ref())
                .and_then(|text| text.attributes.as_ref()),
            current
                .datasheet_field
                .as_ref()
                .and_then(|field| field.text.as_ref())
                .and_then(|text| text.text.as_ref())
                .and_then(|text| text.attributes.as_ref())
        );
        assert_eq!(
            field_text(&updated.description_field),
            "new field description"
        );
        let attributes = updated.attributes.as_ref().unwrap();
        let current_attributes = current.attributes.as_ref().unwrap();
        assert_eq!(
            attributes.not_in_schematic,
            current_attributes.not_in_schematic
        );
        assert_eq!(
            attributes.do_not_populate,
            current_attributes.do_not_populate
        );
        assert!(attributes.exclude_from_position_files);
        assert!(!attributes.exclude_from_bill_of_materials);
        assert_eq!(
            attributes.mounting_style,
            kiapi::board::types::FootprintMountingStyle::FmsSmd as i32
        );
        assert_eq!(updated.overrides, current.overrides);
        assert_eq!(updated.symbol_path, current.symbol_path);
        assert_eq!(updated.symbol_sheet_name, current.symbol_sheet_name);
        assert_eq!(updated.symbol_sheet_filename, current.symbol_sheet_filename);
        assert_eq!(
            updated.symbol_footprint_filters,
            current.symbol_footprint_filters
        );

        let properties = decoded_custom_fields(&updated);
        assert_eq!(
            properties
                .iter()
                .map(|property| (property.name.as_str(), field_text_value(property)))
                .collect::<Vec<_>>(),
            vec![
                ("KiLib_Generator", "konnect_test_generator"),
                ("AssemblyVendor", "Example Assembly"),
            ]
        );
        let assembly = properties
            .iter()
            .find(|property| property.name == "AssemblyVendor")
            .unwrap();
        let assembly_text = assembly.text.as_ref().unwrap();
        assert_eq!(
            assembly_text.layer,
            kiapi::board::types::BoardLayer::BlBFab as i32
        );
        let text = assembly_text.text.as_ref().unwrap();
        let expected = konnect_sexp::geometry::transform_pad(0.5, -0.75, 100.0, 50.0, 37.0);
        let actual = text.position.as_ref().unwrap();
        let actual = (
            builders::nm_to_mm(actual.x_nm),
            builders::nm_to_mm(actual.y_nm),
        );
        assert!((actual.0 - expected.0).abs() <= 0.000_001);
        assert!((actual.1 - expected.1).abs() <= 0.000_001);
        let attributes = text.attributes.as_ref().unwrap();
        assert!(attributes.mirrored);
        assert_eq!(
            attributes.angle.as_ref().unwrap().value_degrees,
            readable_property_angle(180.0 - 15.0 + 37.0)
        );

        let pads = decoded_pads(&updated);
        assert_eq!(pads.iter().filter(|pad| pad.number == "1").count(), 2);
        assert!(pads.iter().filter(|pad| pad.number == "1").all(|pad| pad
            .net
            .as_ref()
            .map(|net| net.name.as_str())
            == Some("ROW1")));
        assert_eq!(
            pads.iter()
                .find(|pad| pad.number == "2")
                .and_then(|pad| pad.net.as_ref())
                .map(|net| net.name.as_str()),
            Some("COL1")
        );
        assert!(pads
            .iter()
            .find(|pad| pad.number == "3")
            .unwrap()
            .net
            .is_none());
        let flipped_pad = pads
            .iter()
            .find(|pad| pad.number == "3")
            .expect("new pad 3");
        let flipped_stack = flipped_pad.pad_stack.as_ref().expect("pad stack");
        assert_eq!(
            flipped_stack.layers,
            vec![kiapi::board::types::BoardLayer::BlBCu as i32],
            "a front-copper library pad moves to back copper with a B.Cu instance"
        );
        let expected = konnect_sexp::geometry::transform_pad(0.0, -3.0, 100.0, 50.0, 37.0);
        let actual = flipped_pad.position.as_ref().expect("pad position");
        let actual = (
            builders::nm_to_mm(actual.x_nm),
            builders::nm_to_mm(actual.y_nm),
        );
        assert!((actual.0 - expected.0).abs() <= 0.000_001);
        assert!((actual.1 - expected.1).abs() <= 0.000_001);
        assert!(prepared.changed_domains.contains(&ChangedDomain::Pads));
        assert!(prepared.changed_domains.contains(&ChangedDomain::Graphics));
        assert!(prepared.changed_domains.contains(&ChangedDomain::Metadata));
        assert!(prepared.changed_domains.contains(&ChangedDomain::Models));
    }

    #[test]
    fn merge_preserves_instance_only_properties_and_refreshes_library_properties() {
        let mut current = current_instance(kiapi::board::types::BoardLayer::BlFCu);
        let instance_only = field("InstanceNote", "keep this", 105.0, 55.0, false);
        let stale_library_property = field("AssemblyVendor", "old library value", 99.0, 49.0, true);
        current.definition.as_mut().unwrap().items.extend([
            builders::pack_any(&instance_only, "kiapi.board.types.Field"),
            builders::pack_any(&stale_library_property, "kiapi.board.types.Field"),
        ]);
        let library = parse_library_footprint("Test:Socket", KICAD_LIBRARY_FOOTPRINT).unwrap();

        let prepared = build_updated_instance(
            &current,
            &library,
            &BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]),
            &BTreeSet::new(),
            false,
            2,
        )
        .unwrap();
        let updated =
            kiapi::board::types::FootprintInstance::decode(prepared.item.value.as_slice()).unwrap();
        let properties = decoded_custom_fields(&updated);

        assert_eq!(
            properties
                .iter()
                .map(|property| property.name.as_str())
                .collect::<Vec<_>>(),
            vec!["InstanceNote", "KiLib_Generator", "AssemblyVendor"]
        );
        assert_eq!(
            properties
                .iter()
                .find(|property| property.name == "InstanceNote")
                .unwrap(),
            &instance_only
        );
        assert_eq!(
            field_text_value(
                properties
                    .iter()
                    .find(|property| property.name == "AssemblyVendor")
                    .unwrap()
            ),
            "Example Assembly"
        );
        assert!(prepared.changed_domains.contains(&ChangedDomain::Metadata));
    }

    #[test]
    fn merge_conflicts_when_library_removes_a_connected_logical_pad() {
        let current = current_instance(kiapi::board::types::BoardLayer::BlFCu);
        let without_pad_two = LIBRARY_FOOTPRINT.replace(
            "  (pad \"2\" thru_hole circle (at 2 0) (size 3 3)\n    (layers \"*.Cu\" \"*.Mask\") (drill 1))\n",
            "",
        );
        let library = parse_library_footprint("Test:Socket", &without_pad_two).unwrap();

        let error = build_updated_instance(
            &current,
            &library,
            &BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]),
            &BTreeSet::from(["COL1".to_string()]),
            false,
            2,
        )
        .unwrap_err();

        assert!(error.to_string().contains("connected pad 2"), "{error:#}");
    }

    #[test]
    fn parser_rejects_unsupported_children_before_any_update_can_be_built() {
        let unsupported = LIBRARY_FOOTPRINT.replace(
            "  (model \"../models/Socket.step\"",
            "  (zone (net 0) (layers \"F.Cu\"))\n  (model \"../models/Socket.step\"",
        );

        let error = parse_library_footprint("Test:Socket", &unsupported).unwrap_err();

        assert!(error.to_string().contains("zone"), "{error:#}");
    }

    #[test]
    fn parser_names_unrepresentable_or_ambiguous_custom_properties() {
        let unsupported = KICAD_LIBRARY_FOOTPRINT.replace(
            "\t(property \"AssemblyVendor\" \"Example Assembly\"\n\t\t(at",
            "\t(property \"AssemblyVendor\" \"Example Assembly\"\n\t\t(unlocked yes)\n\t\t(at",
        );
        assert_ne!(unsupported, KICAD_LIBRARY_FOOTPRINT);
        let error = parse_library_footprint("Test:Socket", &unsupported).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("AssemblyVendor"), "{message}");
        assert!(message.contains("unlocked"), "{message}");

        let duplicate = KICAD_LIBRARY_FOOTPRINT.replace(
            "\"KiLib_Generator\" \"konnect_test_generator\"",
            "\"AssemblyVendor\" \"konnect_test_generator\"",
        );
        assert_ne!(duplicate, KICAD_LIBRARY_FOOTPRINT);
        let error = parse_library_footprint("Test:Socket", &duplicate).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("AssemblyVendor"), "{message}");
        assert!(message.contains("more than once"), "{message}");
    }

    #[test]
    fn mandatory_properties_are_validated_and_duplicate_names_refuse() {
        let mut accepted = Vec::new();
        for (name, value) in [
            ("Reference", "REF**"),
            ("Value", "Socket"),
            ("Datasheet", "new-datasheet.pdf"),
            ("Description", "new field description"),
        ] {
            let authored = format!("\t(property \"{name}\" \"{value}\"\n\t\t(at");
            let unsupported = KICAD_LIBRARY_FOOTPRINT.replace(
                &authored,
                &format!("\t(property \"{name}\" \"{value}\"\n\t\t(unlocked yes)\n\t\t(at"),
            );
            assert_ne!(unsupported, KICAD_LIBRARY_FOOTPRINT);
            match parse_library_footprint("Test:Socket", &unsupported) {
                Ok(_) => accepted.push(format!("{name} unsupported clause")),
                Err(error) => {
                    let message = error.to_string();
                    assert!(message.contains(name), "{message}");
                    assert!(message.contains("unlocked"), "{message}");
                }
            }

            let duplicate = KICAD_LIBRARY_FOOTPRINT.replace(
                "\"KiLib_Generator\" \"konnect_test_generator\"",
                &format!("\"{name}\" \"konnect_test_generator\""),
            );
            assert_ne!(duplicate, KICAD_LIBRARY_FOOTPRINT);
            match parse_library_footprint("Test:Socket", &duplicate) {
                Ok(_) => accepted.push(format!("{name} duplicate name")),
                Err(error) => {
                    let message = error.to_string();
                    assert!(message.contains(name), "{message}");
                    assert!(message.contains("more than once"), "{message}");
                }
            }
        }
        assert!(
            accepted.is_empty(),
            "parser accepted mandatory-property cases that must refuse: {}",
            accepted.join(", ")
        );
    }

    #[test]
    fn repeated_scalar_property_clauses_refuse() {
        let mut accepted = Vec::new();
        let vendor_prefix = "\t(property \"AssemblyVendor\" \"Example Assembly\"\n\t\t(at 0.5 0.75 15)\n\t\t(layer \"F.Fab\")\n\t\t(hide yes)";
        for (clause, replacement) in [
            ("hide", format!("{vendor_prefix}\n\t\t(hide no)")),
            (
                "knockout",
                format!("{vendor_prefix}\n\t\t(knockout yes)\n\t\t(knockout no)"),
            ),
        ] {
            let repeated = KICAD_LIBRARY_FOOTPRINT.replace(vendor_prefix, &replacement);
            assert_ne!(repeated, KICAD_LIBRARY_FOOTPRINT);
            match parse_library_footprint("Test:Socket", &repeated) {
                Ok(_) => accepted.push(clause),
                Err(error) => {
                    let message = error.to_string();
                    assert!(message.contains("AssemblyVendor"), "{message}");
                    assert!(message.contains(clause), "{message}");
                }
            }
        }

        let vendor_font =
            "\t\t\t\t(size 1 1)\n\t\t\t\t(thickness 0.15)\n\t\t\t)\n\t\t)\n\t)\n\t(attr";
        for (clause, repeated_clauses) in [
            ("bold", "\t\t\t\t(bold yes)\n\t\t\t\t(bold no)\n"),
            ("italic", "\t\t\t\t(italic yes)\n\t\t\t\t(italic no)\n"),
            (
                "line_spacing",
                "\t\t\t\t(line_spacing 1.1)\n\t\t\t\t(line_spacing 1.2)\n",
            ),
        ] {
            let replacement =
                vendor_font.replace("\t\t\t)\n", &format!("{repeated_clauses}\t\t\t)\n"));
            let repeated = KICAD_LIBRARY_FOOTPRINT.replace(vendor_font, &replacement);
            assert_ne!(repeated, KICAD_LIBRARY_FOOTPRINT);
            match parse_library_footprint("Test:Socket", &repeated) {
                Ok(_) => accepted.push(clause),
                Err(error) => {
                    let message = error.to_string();
                    assert!(message.contains("AssemblyVendor"), "{message}");
                    assert!(message.contains(clause), "{message}");
                }
            }
        }
        assert!(
            accepted.is_empty(),
            "parser accepted repeated scalar clauses that must refuse: {}",
            accepted.join(", ")
        );
    }

    #[test]
    fn refresh_refuses_an_unrepresentable_layer_before_building_the_update() {
        let unsupported = LIBRARY_FOOTPRINT.replace("(layer \"F.SilkS\"))", "(layer \"In99.Cu\"))");
        let library = parse_library_footprint("Test:Socket", &unsupported).unwrap();
        let current = current_instance(kiapi::board::types::BoardLayer::BlFCu);

        let error = build_updated_instance(
            &current,
            &library,
            &BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]),
            &BTreeSet::new(),
            false,
            2,
        )
        .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("In99.Cu"), "{message}");
        assert!(message.contains("request was not sent"), "{message}");
    }

    #[test]
    fn parser_rejects_lossy_graphic_and_pad_clauses() {
        let unsupported_stroke = LIBRARY_FOOTPRINT.replace(
            "(stroke (width 0.12) (type solid))",
            "(stroke (width 0.12) (type invalid_style))",
        );
        let error = parse_library_footprint("Test:Socket", &unsupported_stroke).unwrap_err();
        assert!(error.to_string().contains("stroke type"), "{error:#}");

        let solder_margin = LIBRARY_FOOTPRINT.replace(
            "(roundrect_rratio 0.2))",
            "(roundrect_rratio 0.2) (solder_mask_margin 0.05))",
        );
        let error = parse_library_footprint("Test:Socket", &solder_margin).unwrap_err();
        assert!(
            error.to_string().contains("solder_mask_margin"),
            "{error:#}"
        );
    }

    #[test]
    fn parser_rejects_lossy_user_text_variants() {
        for (from, to, expected) in [
            (
                "(at 0 1.5 0) (layer \"F.Fab\")",
                "(at 0 1.5 0) (unlocked yes) (layer \"F.Fab\")",
                "unlocked",
            ),
            (
                "(effects (font (size 0.8 0.8) (thickness 0.11)))",
                "(effects (font (size 0.8 0.7) (thickness 0.11)))",
                "non-square",
            ),
            (
                "(effects (font (size 0.8 0.8) (thickness 0.11)))",
                "(effects (font (size 0.8 0.8) (thickness 0.11)) (justify left))",
                "justify",
            ),
            (
                "(effects (font (size 0.8 0.8) (thickness 0.11)))",
                "(effects (font (size 0.8 0.8) (thickness 0.11) (italic yes)))",
                "italic",
            ),
            (
                "(effects (font (size 0.8 0.8) (thickness 0.11)))",
                "(effects (font (size 0.8 0.8)))",
                "missing its thickness",
            ),
            (
                "(uuid \"f96c2efe-5925-4f74-81d2-f89a56f57e13\")",
                "(uuid \"f96c2efe-5925-4f74-81d2-f89a56f57e13\") (uuid \"duplicate\")",
                "multiple identifier clauses",
            ),
            (
                "(uuid \"f96c2efe-5925-4f74-81d2-f89a56f57e13\")",
                "(uuid \"f96c2efe-5925-4f74-81d2-f89a56f57e13\") (tstamp \"legacy-too\")",
                "multiple identifier clauses",
            ),
        ] {
            let unsupported = LIBRARY_FOOTPRINT.replacen(from, to, 1);
            let error = parse_library_footprint("Test:Socket", &unsupported).unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
        }
    }

    #[test]
    fn rebuilding_an_applied_instance_is_a_noop_at_every_rotation_and_side() {
        let library = parse_library_footprint("Test:Socket", KICAD_LIBRARY_FOOTPRINT).unwrap();
        let net_codes = BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]);
        let routed = BTreeSet::from(["ROW1".to_string(), "COL1".to_string()]);

        for layer in [
            kiapi::board::types::BoardLayer::BlFCu,
            kiapi::board::types::BoardLayer::BlBCu,
        ] {
            for rotation in [0.0, 90.0, 180.0, 270.0, 37.0] {
                let mut current = current_instance(layer);
                current.orientation = Some(kiapi::common::types::Angle {
                    value_degrees: rotation,
                });
                let first =
                    build_updated_instance(&current, &library, &net_codes, &routed, false, 2)
                        .unwrap();
                let applied =
                    kiapi::board::types::FootprintInstance::decode(first.item.value.as_slice())
                        .unwrap();

                let second =
                    build_updated_instance(&applied, &library, &net_codes, &routed, false, 2)
                        .unwrap();

                assert!(
                    second.changed_domains.is_empty(),
                    "{layer:?} at {rotation} degrees changed again: {:?}",
                    second.changed_domains
                );
            }
        }
    }

    #[test]
    fn kicad_roundtrip_defaults_do_not_create_false_pad_or_graphic_changes() {
        let library = parse_library_footprint("Test:Socket", LIBRARY_FOOTPRINT).unwrap();
        let current = current_instance(kiapi::board::types::BoardLayer::BlFCu);
        let prepared = build_updated_instance(
            &current,
            &library,
            &BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]),
            &BTreeSet::new(),
            false,
            2,
        )
        .unwrap();
        let mut prepared =
            kiapi::board::types::FootprintInstance::decode(prepared.item.value.as_slice()).unwrap();
        let mut roundtripped = prepared.clone();
        roundtripped.definition.as_mut().unwrap().items.reverse();
        roundtripped.attributes.as_mut().unwrap().mounting_style =
            kiapi::board::types::FootprintMountingStyle::FmsUnspecified as i32;
        prepared.attributes.as_mut().unwrap().mounting_style =
            kiapi::board::types::FootprintMountingStyle::FmsUnknown as i32;

        for item in &mut roundtripped.definition.as_mut().unwrap().items {
            if item.type_url.ends_with("kiapi.board.types.Pad") {
                let mut pad = kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap();
                pad.id = Some(kiapi::common::types::Kiid {
                    value: format!("kicad-pad-{}", pad.number),
                });
                pad.net = Some(kiapi::board::types::Net::default());
                pad.pad_to_die_length = Some(kiapi::common::types::Distance { value_nm: 0 });
                pad.symbol_pin = Some(kiapi::board::types::SymbolPinInfo::default());
                pad.pad_to_die_delay = Some(kiapi::common::types::Time { value_as: 0 });
                let stack = pad.pad_stack.as_mut().unwrap();
                stack.layers.reverse();
                if stack.drill.is_none() {
                    stack.drill = Some(kiapi::board::types::DrillProperties {
                        start_layer: kiapi::board::types::BoardLayer::BlUndefined as i32,
                        end_layer: kiapi::board::types::BoardLayer::BlUndefined as i32,
                        diameter: Some(builders::vec2(0.0, 0.0).unwrap()),
                        shape: kiapi::board::types::DrillShape::DsUndefined as i32,
                        ..Default::default()
                    });
                }
                stack.zone_settings = Some(kiapi::board::types::ZoneConnectionSettings {
                    zone_connection: kiapi::board::types::ZoneConnectionStyle::ZcsInherited as i32,
                    thermal_spokes: Some(kiapi::board::types::ThermalSpokeSettings {
                        angle: Some(kiapi::common::types::Angle {
                            value_degrees: 90.0,
                        }),
                        ..Default::default()
                    }),
                });
                *item = builders::pack_any(&pad, "kiapi.board.types.Pad");
            } else if item
                .type_url
                .ends_with("kiapi.board.types.BoardGraphicShape")
            {
                let mut graphic =
                    kiapi::board::types::BoardGraphicShape::decode(item.value.as_slice()).unwrap();
                graphic.id = Some(kiapi::common::types::Kiid {
                    value: "kicad-graphic".to_string(),
                });
                graphic.net = Some(kiapi::board::types::Net::default());
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
                    .style = kiapi::common::types::StrokeLineStyle::SlsSolid as i32;
                if let Some(kiapi::common::types::graphic_shape::Geometry::Polygon(polyset)) =
                    graphic
                        .shape
                        .as_mut()
                        .and_then(|shape| shape.geometry.as_mut())
                {
                    if let Some(outline) = polyset
                        .polygons
                        .first_mut()
                        .and_then(|polygon| polygon.outline.as_mut())
                    {
                        outline.nodes.rotate_left(1);
                        outline.nodes.reverse();
                    }
                }
                if let Some(kiapi::common::types::graphic_shape::Geometry::Rectangle(rectangle)) =
                    graphic
                        .shape
                        .as_mut()
                        .and_then(|shape| shape.geometry.as_mut())
                {
                    rectangle.corner_radius = Some(kiapi::common::types::Distance { value_nm: 0 });
                }
                *item = builders::pack_any(&graphic, "kiapi.board.types.BoardGraphicShape");
            }
        }

        let roundtripped_pads = normalized_items(roundtripped.definition.as_ref().unwrap(), "Pad")
            .unwrap()
            .into_iter()
            .map(|bytes| kiapi::board::types::Pad::decode(bytes.as_slice()).unwrap())
            .collect::<Vec<_>>();
        let prepared_pads = normalized_items(prepared.definition.as_ref().unwrap(), "Pad")
            .unwrap()
            .into_iter()
            .map(|bytes| kiapi::board::types::Pad::decode(bytes.as_slice()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(roundtripped_pads, prepared_pads);
        assert!(changed_domains(&roundtripped, &prepared)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn issue26_native_inactive_pad_defaults_do_not_change_domains() {
        use kiapi::board::types::{Pad, PadStackShape};

        let instance = kiapi::board::types::FootprintInstance::decode(
            include_bytes!("../../tests/fixtures/issue_474_r1.ipc.bin").as_slice(),
        )
        .unwrap();
        let original = instance.definition.unwrap();
        let mut canonical = original.clone();
        let mut native_defaults = 0;
        for item in &mut canonical.items {
            if !builders::any_is(item, "kiapi.board.types.Pad") {
                continue;
            }
            let mut pad = Pad::decode(item.value.as_slice()).unwrap();
            for layer in &mut pad.pad_stack.as_mut().unwrap().copper_layers {
                assert_ne!(layer.shape, PadStackShape::PssChamferedrect as i32);
                assert_eq!(layer.chamfer_ratio, 0.2);
                native_defaults += 1;
                layer.chamfer_ratio = 0.0;
                layer.chamfered_corners = None;
                if layer.shape != PadStackShape::PssRoundrect as i32 {
                    layer.corner_rounding_ratio = 0.0;
                }
            }
            *item = builders::pack_any(&pad, "kiapi.board.types.Pad");
        }
        assert!(native_defaults > 0);
        assert_eq!(
            normalized_items(&original, "Pad").unwrap(),
            normalized_items(&canonical, "Pad").unwrap()
        );

        for shape in [PadStackShape::PssRoundrect, PadStackShape::PssChamferedrect] {
            let mut baseline = canonical.clone();
            let item = baseline
                .items
                .iter_mut()
                .find(|item| builders::any_is(item, "kiapi.board.types.Pad"))
                .unwrap();
            let mut pad = Pad::decode(item.value.as_slice()).unwrap();
            let layer = &mut pad.pad_stack.as_mut().unwrap().copper_layers[0];
            layer.shape = shape as i32;
            layer.corner_rounding_ratio = 0.2;
            layer.chamfer_ratio = 0.1;
            *item = builders::pack_any(&pad, "kiapi.board.types.Pad");
            let expected = normalized_items(&baseline, "Pad").unwrap();
            for change in 0..3 {
                if change != 0 && shape != PadStackShape::PssChamferedrect {
                    continue;
                }
                let mut changed = baseline.clone();
                let item = changed
                    .items
                    .iter_mut()
                    .find(|item| builders::any_is(item, "kiapi.board.types.Pad"))
                    .unwrap();
                let mut pad = Pad::decode(item.value.as_slice()).unwrap();
                let layer = &mut pad.pad_stack.as_mut().unwrap().copper_layers[0];
                match change {
                    0 => layer.corner_rounding_ratio = 0.3,
                    1 => layer.chamfer_ratio = 0.3,
                    _ => {
                        layer.chamfered_corners = Some(kiapi::board::types::ChamferedRectCorners {
                            top_left: true,
                            ..Default::default()
                        });
                    }
                }
                *item = builders::pack_any(&pad, "kiapi.board.types.Pad");
                assert_ne!(expected, normalized_items(&changed, "Pad").unwrap());
            }
        }
    }

    fn plan_item(reference: &str, library_id: Option<&str>, kiid: &str) -> prost_types::Any {
        let mut instance = current_instance(kiapi::board::types::BoardLayer::BlFCu);
        instance.id = Some(kiapi::common::types::Kiid {
            value: kiid.to_string(),
        });
        instance.reference_field = Some(field("Reference", reference, 101.0, 48.0, true));
        if let Some(definition) = instance.definition.as_mut() {
            definition.reference_field = instance.reference_field.clone();
            definition.id = library_id.map(|library_id| {
                let (nickname, entry) = library_id.split_once(':').unwrap();
                kiapi::common::types::LibraryIdentifier {
                    library_nickname: nickname.to_string(),
                    entry_name: entry.to_string(),
                }
            });
        }
        builders::pack_any(&instance, "kiapi.board.types.FootprintInstance")
    }

    fn plan_fixture() -> (tempfile::TempDir, std::path::PathBuf, Vec<prost_types::Any>) {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb (version 20240108))").unwrap();
        let library_dir = temp.path().join("Test.pretty");
        std::fs::create_dir(&library_dir).unwrap();
        std::fs::write(
            library_dir.join("Socket.kicad_mod"),
            KICAD_LIBRARY_FOOTPRINT,
        )
        .unwrap();
        std::fs::write(
            temp.path().join("fp-lib-table"),
            "(fp_lib_table (lib (name \"Test\") (type \"KiCad\") (uri \"${KIPRJMOD}/Test.pretty\") (options \"\") (descr \"\")))",
        )
        .unwrap();
        let items = vec![
            plan_item("SW2", Some("Test:Socket"), "kiid-2"),
            plan_item("TP1", None, "kiid-3"),
            plan_item("SW1", Some("Test:Socket"), "kiid-1"),
        ];
        (temp, board, items)
    }

    #[test]
    fn planner_refuses_an_unrepresentable_property_before_preparing_any_update() {
        let (temp, board, items) = plan_fixture();
        let unsupported = KICAD_LIBRARY_FOOTPRINT.replace(
            "\t(property \"AssemblyVendor\" \"Example Assembly\"\n\t\t(at",
            "\t(property \"AssemblyVendor\" \"Example Assembly\"\n\t\t(unlocked yes)\n\t\t(at",
        );
        std::fs::write(
            temp.path().join("Test.pretty/Socket.kicad_mod"),
            unsupported,
        )
        .unwrap();

        let plan = plan_updates(
            &board,
            &items,
            &BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]),
            &BTreeSet::new(),
            &UpdateFilters::default(),
            2,
        );

        assert_eq!(plan.status, PlanStatus::Conflict);
        assert!(plan.prepared_items.is_empty());
        assert!(plan.changes.is_empty());
        assert!(plan.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "unsupported_library_footprint"
                && diagnostic.message.contains("AssemblyVendor")
                && diagnostic.message.contains("unlocked")
        }));
    }

    #[test]
    fn filters_are_normalized_and_supplied_empty_arrays_select_nothing() {
        let filters = parse_filters(&serde_json::json!({
            "references": ["SW2", "SW1", "SW2"],
            "library_ids": ["Test:Socket", "Test:Socket"]
        }))
        .unwrap();
        assert_eq!(
            filters.references,
            Some(BTreeSet::from(["SW1".to_string(), "SW2".to_string()]))
        );
        assert_eq!(
            filters.library_ids,
            Some(BTreeSet::from(["Test:Socket".to_string()]))
        );

        let (_, board, items) = plan_fixture();
        let empty = plan_updates(
            &board,
            &items,
            &BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]),
            &BTreeSet::new(),
            &parse_filters(&serde_json::json!({ "references": [] })).unwrap(),
            2,
        );
        assert_eq!(empty.status, PlanStatus::Noop);
        assert_eq!(empty.coverage.selected.planned, 0);
        assert!(empty.changes.is_empty());

        let malformed =
            parse_filters(&serde_json::json!({ "library_ids": ["not-a-library-id"] })).unwrap_err();
        assert_eq!(malformed.field, "library_ids");
        assert!(malformed.reason.contains("Library:Footprint"));
    }

    #[test]
    fn planner_selects_deterministically_and_intersects_filters() {
        let (_temp, board, items) = plan_fixture();
        let net_codes = BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]);
        let all = plan_updates(
            &board,
            &items,
            &net_codes,
            &BTreeSet::new(),
            &UpdateFilters::default(),
            2,
        );
        assert_eq!(all.status, PlanStatus::Ready);
        assert_eq!(all.coverage.selected.planned, 2);
        assert_eq!(all.coverage.changed.planned, 2);
        assert_eq!(all.coverage.skipped_unlinked.planned, 1);
        assert_eq!(
            all.changes
                .iter()
                .map(|change| change.reference.as_str())
                .collect::<Vec<_>>(),
            vec!["SW1", "SW2"]
        );

        let intersection = plan_updates(
            &board,
            &items,
            &net_codes,
            &BTreeSet::new(),
            &parse_filters(&serde_json::json!({
                "references": ["SW2"],
                "library_ids": ["Other:Socket"]
            }))
            .unwrap(),
            2,
        );
        assert_eq!(intersection.status, PlanStatus::Noop);
        assert_eq!(intersection.coverage.selected.planned, 0);
    }

    #[test]
    fn planner_conflicts_on_missing_or_duplicate_selected_references() {
        let (_temp, board, mut items) = plan_fixture();
        let net_codes = BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]);
        let missing = plan_updates(
            &board,
            &items,
            &net_codes,
            &BTreeSet::new(),
            &parse_filters(&serde_json::json!({ "references": ["SW404"] })).unwrap(),
            2,
        );
        assert_eq!(missing.status, PlanStatus::Conflict);
        assert!(missing
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "reference_not_found"));

        items.push(plan_item("SW1", Some("Test:Socket"), "kiid-duplicate"));
        let duplicate = plan_updates(
            &board,
            &items,
            &net_codes,
            &BTreeSet::new(),
            &parse_filters(&serde_json::json!({ "references": ["SW1"] })).unwrap(),
            2,
        );
        assert_eq!(duplicate.status, PlanStatus::Conflict);
        assert!(duplicate
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "duplicate_reference"));
        assert!(duplicate.prepared_items.is_empty());
    }

    #[test]
    fn explicitly_selected_unlinked_footprint_is_a_conflict() {
        let (_temp, board, items) = plan_fixture();
        let plan = plan_updates(
            &board,
            &items,
            &BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]),
            &BTreeSet::new(),
            &parse_filters(&serde_json::json!({ "references": ["TP1"] })).unwrap(),
            2,
        );

        assert_eq!(plan.status, PlanStatus::Conflict);
        assert!(plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "unlinked_footprint"));
        assert!(plan.prepared_items.is_empty());
    }

    #[test]
    fn planner_returns_noop_after_the_prepared_update_is_applied() {
        let (_temp, board, items) = plan_fixture();
        let net_codes = BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]);
        let first = plan_updates(
            &board,
            &[items[2].clone()],
            &net_codes,
            &BTreeSet::new(),
            &UpdateFilters::default(),
            2,
        );
        assert_eq!(first.status, PlanStatus::Ready);

        let second = plan_updates(
            &board,
            &first.prepared_items,
            &net_codes,
            &BTreeSet::new(),
            &UpdateFilters::default(),
            2,
        );

        assert_eq!(second.status, PlanStatus::Noop);
        assert_eq!(second.coverage.selected.planned, 1);
        assert_eq!(second.coverage.changed.planned, 0);
        assert_eq!(second.coverage.unchanged.planned, 1);
        assert!(second.prepared_items.is_empty());
    }

    #[test]
    fn datasheet_only_library_change_is_reported_as_metadata() {
        let base = parse_library_footprint("Test:Socket", LIBRARY_FOOTPRINT).unwrap();
        let current = current_instance(kiapi::board::types::BoardLayer::BlFCu);
        let net_codes = BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]);
        let routed = BTreeSet::new();
        let applied =
            build_updated_instance(&current, &base, &net_codes, &routed, false, 2).unwrap();
        let applied =
            kiapi::board::types::FootprintInstance::decode(applied.item.value.as_slice()).unwrap();
        let changed_source =
            LIBRARY_FOOTPRINT.replace("new-datasheet.pdf", "replacement-datasheet.pdf");
        let changed = parse_library_footprint("Test:Socket", &changed_source).unwrap();

        let prepared =
            build_updated_instance(&applied, &changed, &net_codes, &routed, false, 2).unwrap();

        assert_eq!(
            prepared.changed_domains,
            BTreeSet::from([ChangedDomain::Metadata])
        );
    }

    #[test]
    fn plan_revision_tracks_selected_board_library_and_filter_inputs_only() {
        let (_temp, board, mut items) = plan_fixture();
        let net_codes = BTreeMap::from([("ROW1".to_string(), 11), ("COL1".to_string(), 12)]);
        let filters = parse_filters(&serde_json::json!({ "references": ["SW1"] })).unwrap();
        let first = plan_updates(&board, &items, &net_codes, &BTreeSet::new(), &filters, 2);
        let identical = plan_updates(&board, &items, &net_codes, &BTreeSet::new(), &filters, 2);
        assert_eq!(first.plan_revision, identical.plan_revision);

        items[0] = plan_item("SW2", Some("Test:Socket"), "unselected-changed");
        let unrelated = plan_updates(&board, &items, &net_codes, &BTreeSet::new(), &filters, 2);
        assert_eq!(first.plan_revision, unrelated.plan_revision);

        items[2] = plan_item("SW1", Some("Test:Socket"), "selected-changed");
        let board_changed = plan_updates(&board, &items, &net_codes, &BTreeSet::new(), &filters, 2);
        assert_ne!(first.plan_revision, board_changed.plan_revision);

        std::fs::write(
            board.parent().unwrap().join("Test.pretty/Socket.kicad_mod"),
            LIBRARY_FOOTPRINT.replace("updated description", "library changed"),
        )
        .unwrap();
        let library_changed =
            plan_updates(&board, &items, &net_codes, &BTreeSet::new(), &filters, 2);
        assert_ne!(board_changed.plan_revision, library_changed.plan_revision);

        let different_filter = plan_updates(
            &board,
            &items,
            &net_codes,
            &BTreeSet::new(),
            &parse_filters(&serde_json::json!({ "references": ["SW2"] })).unwrap(),
            2,
        );
        assert_ne!(
            library_changed.plan_revision,
            different_filter.plan_revision
        );
    }

    fn test_context(ipc_address: &str) -> crate::tools::ToolContext {
        crate::tools::ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: ipc_address.to_string(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        )
    }

    fn result_json(result: &crate::mcp::protocol::CallToolResult) -> serde_json::Value {
        let text = match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => text,
            other => panic!("expected text result, got {other:?}"),
        };
        serde_json::from_str(text).expect("tool result must be JSON")
    }

    #[test]
    fn tool_schema_defaults_to_dry_run_and_requires_only_the_board() {
        let definition = tool();

        assert_eq!(definition.name, "update_footprints_from_library");
        assert_eq!(
            definition.input_schema["required"],
            serde_json::json!(["board"])
        );
        assert_eq!(
            definition.input_schema["properties"]["dry_run"]["default"],
            true
        );
        assert_eq!(
            definition.input_schema["properties"]["references"]["items"]["type"],
            "string"
        );
        assert_eq!(
            definition.input_schema["properties"]["library_ids"]["items"]["type"],
            "string"
        );
    }

    #[tokio::test]
    async fn apply_without_a_reviewed_revision_is_rejected_before_ipc() {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb (version 20240108))").unwrap();

        let result = handle_update_footprints_from_library(
            &serde_json::json!({ "board": board, "dry_run": false }),
            &test_context(""),
        )
        .await
        .unwrap();

        assert!(result.is_error);
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("invalid_argument")
        );
        let text = match result.content.first().unwrap() {
            crate::mcp::protocol::ToolContent::Text { text } => text,
            _ => unreachable!(),
        };
        assert!(text.contains("expected_plan_revision"));
    }

    #[tokio::test]
    async fn unreachable_ipc_returns_a_non_mutating_conflict_response() {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        let before = "(kicad_pcb (version 20240108))";
        std::fs::write(&board, before).unwrap();

        let result = handle_update_footprints_from_library(
            &serde_json::json!({ "board": board }),
            &test_context(""),
        )
        .await
        .unwrap();

        assert!(!result.is_error);
        let response = result_json(&result);
        assert_eq!(response["status"], "conflict");
        assert_eq!(response["coverage"]["transport"], "live_kicad_ipc");
        assert!(response["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("live-IPC-only"));
        assert_eq!(std::fs::read_to_string(board).unwrap(), before);
    }

    #[derive(Default)]
    struct MutationCapture {
        begin_count: usize,
        update_batches: Vec<usize>,
        commit_actions: Vec<kiapi::common::commands::CommitAction>,
    }

    #[derive(Clone, Copy, Default)]
    enum ReadbackFault {
        #[default]
        None,
        WrongLayer,
        MissingTextMirror,
        MissingFootprint,
        Unavailable,
        RollbackFailure,
    }

    struct PlannerMock {
        url: String,
        live_source: std::sync::Arc<std::sync::Mutex<String>>,
        readback_fault: std::sync::Arc<std::sync::Mutex<ReadbackFault>>,
        copper_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
        capture: std::sync::Arc<std::sync::Mutex<MutationCapture>>,
        _thread: std::thread::JoinHandle<()>,
    }

    fn api_ok() -> kiapi::common::ApiResponse {
        kiapi::common::ApiResponse {
            status: Some(kiapi::common::ApiResponseStatus {
                status: kiapi::common::ApiStatusCode::AsOk as i32,
                error_message: String::new(),
            }),
            header: None,
            message: None,
        }
    }

    fn api_reply(message: prost_types::Any) -> kiapi::common::ApiResponse {
        kiapi::common::ApiResponse {
            message: Some(message),
            ..api_ok()
        }
    }

    #[derive(Clone, Copy)]
    enum ChamferReadback {
        Faithful,
        DropCorners,
        Missing,
        Unreadable,
        Unavailable,
        RollbackFailure,
        MissingAfterCommit,
        FieldsAfterCommit,
        StyleAfterCommit,
    }

    fn api_refused(message: &str) -> kiapi::common::ApiResponse {
        kiapi::common::ApiResponse {
            status: Some(kiapi::common::ApiResponseStatus {
                status: kiapi::common::ApiStatusCode::AsBadRequest as i32,
                error_message: message.to_string(),
            }),
            ..Default::default()
        }
    }

    fn spawn_planner_mock(
        board: &Path,
        footprint: prost_types::Any,
        fail_update: bool,
    ) -> PlannerMock {
        spawn_planner_mock_readback(board, footprint, fail_update, ChamferReadback::Faithful)
    }

    fn spawn_planner_mock_readback(
        board: &Path,
        footprint: prost_types::Any,
        fail_update: bool,
        readback: ChamferReadback,
    ) -> PlannerMock {
        use nng::options::Options;

        static NEXT_MOCK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let url = format!(
            "inproc://footprint-update-{}",
            NEXT_MOCK.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        socket
            .set_opt::<nng::options::RecvTimeout>(Some(std::time::Duration::from_secs(20)))
            .unwrap();
        socket.listen(&url).unwrap();

        let board_name = board.to_string_lossy().to_string();
        let capture = std::sync::Arc::new(std::sync::Mutex::new(MutationCapture::default()));
        let captured = capture.clone();
        let live_source = std::sync::Arc::new(std::sync::Mutex::new(
            std::fs::read_to_string(board).unwrap(),
        ));
        let snapshot = live_source.clone();
        let copper_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(2));
        let live_copper_count = copper_count.clone();
        let readback_fault = std::sync::Arc::new(std::sync::Mutex::new(ReadbackFault::None));
        let live_readback_fault = readback_fault.clone();
        let thread = std::thread::spawn(move || {
            let original = footprint.clone();
            let mut footprint = footprint;
            let mut update_applied = false;
            while let Ok(message) = socket.recv() {
                let request = kiapi::common::ApiRequest::decode(message.as_slice()).unwrap();
                let message = request.message.expect("request message");
                let response = if message.type_url.ends_with("GetOpenDocuments") {
                    api_reply(builders::pack_any(
                        &kiapi::common::commands::GetOpenDocumentsResponse {
                            documents: vec![kiapi::common::types::DocumentSpecifier {
                                r#type: kiapi::common::types::DocumentType::DoctypePcb as i32,
                                identifier: Some(
                                    kiapi::common::types::document_specifier::Identifier::BoardFilename(
                                        board_name.clone(),
                                    ),
                                ),
                                project: None,
                            }],
                        },
                        "kiapi.common.commands.GetOpenDocumentsResponse",
                    ))
                } else if message.type_url.ends_with("SaveDocumentToString") {
                    let request = kiapi::common::commands::SaveDocumentToString::decode(
                        message.value.as_slice(),
                    )
                    .unwrap();
                    api_reply(builders::pack_any(
                        &kiapi::common::commands::SavedDocumentResponse {
                            document: request.document,
                            contents: snapshot.lock().unwrap().clone(),
                        },
                        "kiapi.common.commands.SavedDocumentResponse",
                    ))
                } else if message.type_url.ends_with("GetBoardEnabledLayers") {
                    api_reply(builders::pack_any(
                        &kiapi::board::commands::BoardEnabledLayersResponse {
                            layers: vec![
                                kiapi::board::types::BoardLayer::BlFCu as i32,
                                kiapi::board::types::BoardLayer::BlBCu as i32,
                            ],
                            copper_layer_count: live_copper_count
                                .load(std::sync::atomic::Ordering::Relaxed),
                        },
                        "kiapi.board.commands.BoardEnabledLayersResponse",
                    ))
                } else if message.type_url.ends_with("GetItems") {
                    let command =
                        kiapi::common::commands::GetItems::decode(message.value.as_slice())
                            .unwrap();
                    assert_eq!(
                        command
                            .header
                            .as_ref()
                            .unwrap()
                            .document
                            .as_ref()
                            .unwrap()
                            .identifier,
                        Some(
                            kiapi::common::types::document_specifier::Identifier::BoardFilename(
                                board_name.clone()
                            )
                        )
                    );
                    if update_applied && matches!(readback, ChamferReadback::Unavailable) {
                        socket
                            .send(nng::Message::from(
                                api_refused("readback unavailable")
                                    .encode_to_vec()
                                    .as_slice(),
                            ))
                            .unwrap();
                        continue;
                    }
                    let published = captured
                        .lock()
                        .unwrap()
                        .commit_actions
                        .contains(&kiapi::common::commands::CommitAction::CmaCommit);
                    let mut returned = footprint.clone();
                    if published
                        && matches!(
                            readback,
                            ChamferReadback::FieldsAfterCommit | ChamferReadback::StyleAfterCommit
                        )
                    {
                        let mut instance = kiapi::board::types::FootprintInstance::decode(
                            returned.value.as_slice(),
                        )
                        .unwrap();
                        let wanted = if matches!(readback, ChamferReadback::FieldsAfterCommit) {
                            "kiapi.board.types.Field"
                        } else {
                            "kiapi.board.types.BoardGraphicShape"
                        };
                        let child = instance
                            .definition
                            .as_mut()
                            .unwrap()
                            .items
                            .iter_mut()
                            .find(|item| builders::any_is(item, wanted))
                            .unwrap();
                        if matches!(readback, ChamferReadback::FieldsAfterCommit) {
                            let mut field =
                                kiapi::board::types::Field::decode(child.value.as_slice()).unwrap();
                            field.visible = !field.visible;
                            *child = builders::pack_any(&field, wanted);
                        } else {
                            let mut shape = kiapi::board::types::BoardGraphicShape::decode(
                                child.value.as_slice(),
                            )
                            .unwrap();
                            shape
                                .shape
                                .as_mut()
                                .unwrap()
                                .attributes
                                .as_mut()
                                .unwrap()
                                .stroke
                                .as_mut()
                                .unwrap()
                                .style = kiapi::common::types::StrokeLineStyle::SlsDash as i32;
                            *child = builders::pack_any(&shape, wanted);
                        }
                        returned =
                            builders::pack_any(&instance, "kiapi.board.types.FootprintInstance");
                    }
                    let items = if command.types
                        == vec![kiapi::common::types::KiCadObjectType::KotPcbFootprint as i32]
                    {
                        let fault = *live_readback_fault.lock().unwrap();
                        if (update_applied
                            && (matches!(fault, ReadbackFault::MissingFootprint)
                                || matches!(readback, ChamferReadback::Missing)))
                            || (published
                                && matches!(readback, ChamferReadback::MissingAfterCommit))
                        {
                            Vec::new()
                        } else {
                            let mut observed = returned;
                            if update_applied
                                && matches!(
                                    fault,
                                    ReadbackFault::WrongLayer | ReadbackFault::RollbackFailure
                                )
                            {
                                let mut decoded = kiapi::board::types::FootprintInstance::decode(
                                    observed.value.as_slice(),
                                )
                                .unwrap();
                                decoded.layer = kiapi::board::types::BoardLayer::BlFCu as i32;
                                observed = builders::pack_any(
                                    &decoded,
                                    "kiapi.board.types.FootprintInstance",
                                );
                            }
                            if update_applied && matches!(fault, ReadbackFault::MissingTextMirror) {
                                let mut decoded = kiapi::board::types::FootprintInstance::decode(
                                    observed.value.as_slice(),
                                )
                                .unwrap();
                                decoded
                                    .reference_field
                                    .as_mut()
                                    .unwrap()
                                    .text
                                    .as_mut()
                                    .unwrap()
                                    .text
                                    .as_mut()
                                    .unwrap()
                                    .attributes
                                    .as_mut()
                                    .unwrap()
                                    .mirrored = false;
                                observed = builders::pack_any(
                                    &decoded,
                                    "kiapi.board.types.FootprintInstance",
                                );
                            }
                            vec![observed]
                        }
                    } else {
                        Vec::new()
                    };
                    if update_applied
                        && matches!(
                            *live_readback_fault.lock().unwrap(),
                            ReadbackFault::Unavailable
                        )
                    {
                        kiapi::common::ApiResponse {
                            status: Some(kiapi::common::ApiResponseStatus {
                                status: kiapi::common::ApiStatusCode::AsUnhandled as i32,
                                error_message: "readback unavailable".to_string(),
                            }),
                            ..api_ok()
                        }
                    } else {
                        api_reply(builders::pack_any(
                            &kiapi::common::commands::GetItemsResponse {
                                header: None,
                                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                                items,
                            },
                            "kiapi.common.commands.GetItemsResponse",
                        ))
                    }
                } else if message.type_url.ends_with("GetNets") {
                    api_reply(builders::pack_any(
                        &kiapi::board::commands::NetsResponse {
                            nets: vec![builders::net("ROW1", 11), builders::net("COL1", 12)],
                        },
                        "kiapi.board.commands.NetsResponse",
                    ))
                } else if message.type_url.ends_with("BeginCommit") {
                    captured.lock().unwrap().begin_count += 1;
                    api_reply(builders::pack_any(
                        &kiapi::common::commands::BeginCommitResponse {
                            id: Some(kiapi::common::types::Kiid {
                                value: "commit-1".to_string(),
                            }),
                        },
                        "kiapi.common.commands.BeginCommitResponse",
                    ))
                } else if message.type_url.ends_with("UpdateItems") {
                    let update =
                        kiapi::common::commands::UpdateItems::decode(message.value.as_slice())
                            .unwrap();
                    captured
                        .lock()
                        .unwrap()
                        .update_batches
                        .push(update.items.len());
                    if !fail_update {
                        update_applied = true;
                        footprint = update.items[0].clone();
                        if matches!(
                            readback,
                            ChamferReadback::DropCorners
                                | ChamferReadback::RollbackFailure
                                | ChamferReadback::Unreadable
                        ) {
                            let mut instance = kiapi::board::types::FootprintInstance::decode(
                                footprint.value.as_slice(),
                            )
                            .unwrap();
                            for child in &mut instance.definition.as_mut().unwrap().items {
                                if builders::any_is(child, "kiapi.board.types.Pad") {
                                    let mut pad =
                                        kiapi::board::types::Pad::decode(child.value.as_slice())
                                            .unwrap();
                                    if pad.number == "4" {
                                        if matches!(readback, ChamferReadback::Unreadable) {
                                            child.value = vec![0xff];
                                        } else {
                                            for layer in
                                                &mut pad.pad_stack.as_mut().unwrap().copper_layers
                                            {
                                                layer.chamfered_corners = None;
                                            }
                                            *child =
                                                builders::pack_any(&pad, "kiapi.board.types.Pad");
                                        }
                                    }
                                }
                            }
                            footprint = builders::pack_any(
                                &instance,
                                "kiapi.board.types.FootprintInstance",
                            );
                        }
                    }
                    let updated_items = update
                        .items
                        .into_iter()
                        .map(|item| kiapi::common::commands::ItemUpdateResult {
                            status: Some(kiapi::common::commands::ItemStatus {
                                code: if fail_update {
                                    kiapi::common::commands::ItemStatusCode::IscInvalidData as i32
                                } else {
                                    kiapi::common::commands::ItemStatusCode::IscOk as i32
                                },
                                error_message: if fail_update {
                                    "mock update failure".to_string()
                                } else {
                                    String::new()
                                },
                            }),
                            item: Some(item),
                        })
                        .collect();
                    api_reply(builders::pack_any(
                        &kiapi::common::commands::UpdateItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            updated_items,
                        },
                        "kiapi.common.commands.UpdateItemsResponse",
                    ))
                } else if message.type_url.ends_with("EndCommit") {
                    let end = kiapi::common::commands::EndCommit::decode(message.value.as_slice())
                        .unwrap();
                    captured.lock().unwrap().commit_actions.push(end.action());
                    if end.action() == kiapi::common::commands::CommitAction::CmaDrop {
                        if matches!(
                            *live_readback_fault.lock().unwrap(),
                            ReadbackFault::RollbackFailure
                        ) || matches!(readback, ChamferReadback::RollbackFailure)
                        {
                            socket
                                .send(nng::Message::from(
                                    kiapi::common::ApiResponse {
                                        status: Some(kiapi::common::ApiResponseStatus {
                                            status: kiapi::common::ApiStatusCode::AsUnhandled
                                                as i32,
                                            error_message: "rollback unavailable".to_string(),
                                        }),
                                        ..api_ok()
                                    }
                                    .encode_to_vec()
                                    .as_slice(),
                                ))
                                .unwrap();
                            continue;
                        }
                        footprint = original.clone();
                        update_applied = false;
                    }
                    api_reply(builders::pack_any(
                        &kiapi::common::commands::EndCommitResponse {},
                        "kiapi.common.commands.EndCommitResponse",
                    ))
                } else {
                    panic!("unexpected mock request {}", message.type_url);
                };
                let response = nng::Message::from(response.encode_to_vec().as_slice());
                if socket.send(response).is_err() {
                    break;
                }
            }
        });
        PlannerMock {
            url,
            live_source,
            readback_fault,
            copper_count,
            capture,
            _thread: thread,
        }
    }

    #[tokio::test]
    async fn issue26_postpublication_missing_fields_or_styles_remain_uncertain() {
        for mode in [
            ChamferReadback::MissingAfterCommit,
            ChamferReadback::FieldsAfterCommit,
            ChamferReadback::StyleAfterCommit,
        ] {
            let (_temp, board, items) = plan_fixture();
            let server = spawn_planner_mock_readback(&board, items[2].clone(), false, mode);
            let context = test_context(&server.url);
            let dry = handle_update_footprints_from_library(&json!({"board":board}), &context)
                .await
                .unwrap();
            let revision = result_json(&dry)["plan_revision"]
                .as_str()
                .unwrap()
                .to_string();
            let result = handle_update_footprints_from_library(
                &json!({"board":board,"dry_run":false,"expected_plan_revision":revision}),
                &context,
            )
            .await
            .unwrap();
            assert!(result.is_error);
            assert_eq!(
                crate::mcp::error::extract_error_kind(&result).as_deref(),
                Some("ipc_outcome_unknown")
            );
            let capture = server.capture.lock().unwrap();
            assert_eq!(capture.update_batches, vec![1]);
            assert_eq!(
                capture.commit_actions,
                vec![kiapi::common::commands::CommitAction::CmaCommit]
            );
        }
    }

    #[tokio::test]
    async fn issue26_unsaved_group_membership_refuses_before_any_mutation() {
        let (_temp, board, items) = plan_fixture();
        let id = kiapi::board::types::FootprintInstance::decode(items[2].value.as_slice())
            .unwrap()
            .id
            .unwrap()
            .value;
        let server = spawn_planner_mock(&board, items[2].clone(), false);
        *server.live_source.lock().unwrap() = format!(
            "(kicad_pcb (group \"Unsaved assembly\" (members {})))",
            konnect_sexp::quoted(&id)
        );
        let before = std::fs::read(&board).unwrap();
        let result = handle_update_footprints_from_library(
            &json!({"board":board}),
            &test_context(&server.url),
        )
        .await
        .unwrap();
        let response = result_json(&result);
        assert_eq!(response["status"], "conflict");
        assert!(response["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Unsaved assembly"));
        let capture = server.capture.lock().unwrap();
        assert_eq!(capture.begin_count, 0);
        assert!(capture.update_batches.is_empty());
        assert_eq!(std::fs::read(board).unwrap(), before);
    }

    #[tokio::test]
    async fn live_stack_change_invalidates_a_reviewed_library_refresh_before_mutation() {
        let (_temp, board, items) = plan_fixture();
        let mock = spawn_planner_mock(&board, items[2].clone(), false);
        mock.copper_count
            .store(8, std::sync::atomic::Ordering::Relaxed);
        let context = test_context(&mock.url);
        let dry_run =
            handle_update_footprints_from_library(&serde_json::json!({"board": board}), &context)
                .await
                .unwrap();
        let dry_run = result_json(&dry_run);
        assert_eq!(dry_run["status"], "ready");
        mock.copper_count
            .store(6, std::sync::atomic::Ordering::Relaxed);
        let applied = handle_update_footprints_from_library(&serde_json::json!({
            "board": board, "dry_run": false, "expected_plan_revision": dry_run["plan_revision"],
        }), &context).await.unwrap();
        let applied = result_json(&applied);
        assert_eq!(applied["status"], "conflict");
        assert_eq!(applied["diagnostics"][0]["code"], "stale_plan_revision");
        let capture = mock.capture.lock().unwrap();
        assert_eq!(capture.begin_count, 0);
        assert!(capture.update_batches.is_empty());
    }

    #[tokio::test]
    async fn live_refresh_refuses_an_unavailable_copper_count_before_mutation() {
        let (_temp, board, items) = plan_fixture();
        let mock = spawn_planner_mock(&board, items[2].clone(), false);
        mock.copper_count
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let result = handle_update_footprints_from_library(
            &serde_json::json!({"board": board}),
            &test_context(&mock.url),
        )
        .await
        .unwrap();
        let result = result_json(&result);
        assert_eq!(result["status"], "conflict");
        assert!(result["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("copper layer count 0"));
        assert_eq!(mock.capture.lock().unwrap().begin_count, 0);
    }

    #[tokio::test]
    async fn dry_run_and_stale_revision_never_open_a_commit_or_send_updates() {
        let (_temp, board, items) = plan_fixture();
        let mock = spawn_planner_mock(&board, items[2].clone(), false);
        let context = test_context(&mock.url);

        let dry_run =
            handle_update_footprints_from_library(&serde_json::json!({ "board": board }), &context)
                .await
                .unwrap();
        assert_eq!(result_json(&dry_run)["status"], "ready");

        let stale = handle_update_footprints_from_library(
            &serde_json::json!({
                "board": board,
                "dry_run": false,
                "expected_plan_revision": "stale"
            }),
            &context,
        )
        .await
        .unwrap();
        let stale = result_json(&stale);
        assert_eq!(stale["status"], "conflict");
        assert_eq!(stale["diagnostics"][0]["code"], "stale_plan_revision");

        let capture = mock.capture.lock().unwrap();
        assert_eq!(capture.begin_count, 0);
        assert!(capture.update_batches.is_empty());
        assert!(capture.commit_actions.is_empty());
    }

    #[tokio::test]
    async fn apply_sends_one_batch_in_one_commit() {
        let (_temp, board, items) = plan_fixture();
        let mock = spawn_planner_mock(&board, items[2].clone(), false);
        let context = test_context(&mock.url);
        let dry_run =
            handle_update_footprints_from_library(&serde_json::json!({ "board": board }), &context)
                .await
                .unwrap();
        let revision = result_json(&dry_run)["plan_revision"]
            .as_str()
            .unwrap()
            .to_string();

        let applied = handle_update_footprints_from_library(
            &serde_json::json!({
                "board": board,
                "dry_run": false,
                "expected_plan_revision": revision
            }),
            &context,
        )
        .await
        .unwrap();

        let applied = result_json(&applied);
        assert_eq!(applied["status"], "applied");
        assert_eq!(applied["coverage"]["changed"]["applied"], 1);
        let capture = mock.capture.lock().unwrap();
        assert_eq!(capture.begin_count, 1);
        assert_eq!(capture.update_batches, vec![1]);
        assert_eq!(
            capture.commit_actions,
            vec![kiapi::common::commands::CommitAction::CmaCommit]
        );
    }

    #[tokio::test]
    async fn refresh_verifies_back_side_chamfer_readback_before_committing() {
        for behavior in [
            ChamferReadback::Faithful,
            ChamferReadback::DropCorners,
            ChamferReadback::Missing,
            ChamferReadback::Unreadable,
            ChamferReadback::Unavailable,
            ChamferReadback::RollbackFailure,
        ] {
            let (temp, board, items) = plan_fixture();
            let source = KICAD_LIBRARY_FOOTPRINT;
            let end = source.rfind(')').unwrap();
            let source = format!("{} (pad \"4\" smd chamfered_rect (at 1 2 30) (size 2 1) (layers \"F.Cu\") (chamfer_ratio 0.2) (chamfer top_left)){}", &source[..end], &source[end..]);
            std::fs::write(temp.path().join("Test.pretty/Socket.kicad_mod"), source).unwrap();
            let mut instance =
                kiapi::board::types::FootprintInstance::decode(items[2].value.as_slice()).unwrap();
            instance.layer = kiapi::board::types::BoardLayer::BlBCu as i32;
            let item = builders::pack_any(&instance, "kiapi.board.types.FootprintInstance");
            let original = item.clone();
            let mock = spawn_planner_mock_readback(&board, item, false, behavior);
            let context = test_context(&mock.url);
            let dry_run = handle_update_footprints_from_library(&json!({"board": board}), &context)
                .await
                .unwrap();
            let dry_run = result_json(&dry_run);
            assert_eq!(dry_run["status"], "ready", "{dry_run}");
            let applied = handle_update_footprints_from_library(
                &json!({
                    "board": board, "dry_run": false,
                    "expected_plan_revision": dry_run["plan_revision"],
                }),
                &context,
            )
            .await
            .unwrap();
            let observed = result_json(&applied);
            if matches!(behavior, ChamferReadback::Faithful) {
                assert_eq!(observed["status"], "applied", "{observed}");
            } else {
                assert!(applied.is_error, "{observed}");
                if matches!(behavior, ChamferReadback::RollbackFailure) {
                    assert_eq!(
                        crate::mcp::error::extract_error_kind(&applied).as_deref(),
                        Some("ipc_outcome_unknown")
                    );
                } else {
                    assert!(
                        observed["message"]
                            .as_str()
                            .unwrap()
                            .contains("rolled back"),
                        "{observed}"
                    );
                    let client = konnect_ipc::KiCadIpcClient::new(mock.url.clone());
                    let document = client.find_open_board(&board).unwrap();
                    let restored = client
                        .get_items_in(
                            document,
                            kiapi::common::types::KiCadObjectType::KotPcbFootprint,
                        )
                        .unwrap();
                    assert_eq!(restored, vec![original]);
                }
            }
            let capture = mock.capture.lock().unwrap();
            assert_eq!(capture.update_batches, vec![1]);
            assert_eq!(
                capture.commit_actions,
                vec![if matches!(behavior, ChamferReadback::Faithful) {
                    kiapi::common::commands::CommitAction::CmaCommit
                } else {
                    kiapi::common::commands::CommitAction::CmaDrop
                }]
            );
        }
    }

    #[tokio::test]
    async fn library_refresh_readback_failure_drops_the_commit() {
        for fault in [
            ReadbackFault::WrongLayer,
            ReadbackFault::MissingTextMirror,
            ReadbackFault::MissingFootprint,
            ReadbackFault::Unavailable,
        ] {
            let (_temp, board, items) = plan_fixture();
            let mut current =
                kiapi::board::types::FootprintInstance::decode(items[2].value.as_slice()).unwrap();
            current.layer = kiapi::board::types::BoardLayer::BlBCu as i32;
            current
                .reference_field
                .as_mut()
                .unwrap()
                .text
                .as_mut()
                .unwrap()
                .text
                .as_mut()
                .unwrap()
                .attributes
                .as_mut()
                .unwrap()
                .mirrored = true;
            let mock = spawn_planner_mock(
                &board,
                builders::pack_any(&current, "kiapi.board.types.FootprintInstance"),
                false,
            );
            let context = test_context(&mock.url);
            let dry_run = handle_update_footprints_from_library(
                &serde_json::json!({ "board": board }),
                &context,
            )
            .await
            .unwrap();
            assert_eq!(result_json(&dry_run)["status"], "ready");
            *mock.readback_fault.lock().unwrap() = fault;
            let failed = handle_update_footprints_from_library(
                &serde_json::json!({
                    "board": board, "dry_run": false,
                    "expected_plan_revision": result_json(&dry_run)["plan_revision"]
                }),
                &context,
            )
            .await
            .unwrap();
            assert_eq!(result_json(&failed)["status"], "conflict");
            let capture = mock.capture.lock().unwrap();
            assert_eq!(capture.update_batches, [1]);
            assert_eq!(
                capture.commit_actions,
                [kiapi::common::commands::CommitAction::CmaDrop]
            );
        }
    }

    #[tokio::test]
    async fn library_refresh_rollback_failure_preserves_uncertain_outcome() {
        let (_temp, board, items) = plan_fixture();
        let mut current =
            kiapi::board::types::FootprintInstance::decode(items[2].value.as_slice()).unwrap();
        current.layer = kiapi::board::types::BoardLayer::BlBCu as i32;
        let mock = spawn_planner_mock(
            &board,
            builders::pack_any(&current, "kiapi.board.types.FootprintInstance"),
            false,
        );
        let context = test_context(&mock.url);
        let dry_run =
            handle_update_footprints_from_library(&serde_json::json!({ "board": board }), &context)
                .await
                .unwrap();
        *mock.readback_fault.lock().unwrap() = ReadbackFault::RollbackFailure;
        let failed = handle_update_footprints_from_library(
            &serde_json::json!({
                "board": board, "dry_run": false,
                "expected_plan_revision": result_json(&dry_run)["plan_revision"]
            }),
            &context,
        )
        .await
        .unwrap();
        assert!(failed.is_error);
        assert_eq!(
            crate::mcp::error::extract_error_kind(&failed).as_deref(),
            Some("ipc_outcome_unknown")
        );
        assert_eq!(mock.capture.lock().unwrap().update_batches, [1]);
    }

    #[tokio::test]
    async fn failed_update_drops_the_single_commit() {
        let (_temp, board, items) = plan_fixture();
        let mock = spawn_planner_mock(&board, items[2].clone(), true);
        let context = test_context(&mock.url);
        let dry_run =
            handle_update_footprints_from_library(&serde_json::json!({ "board": board }), &context)
                .await
                .unwrap();
        let revision = result_json(&dry_run)["plan_revision"]
            .as_str()
            .unwrap()
            .to_string();

        let failed = handle_update_footprints_from_library(
            &serde_json::json!({
                "board": board,
                "dry_run": false,
                "expected_plan_revision": revision
            }),
            &context,
        )
        .await
        .unwrap();

        assert_eq!(result_json(&failed)["status"], "conflict");
        let capture = mock.capture.lock().unwrap();
        assert_eq!(capture.begin_count, 1);
        assert_eq!(capture.update_batches, vec![1]);
        assert_eq!(
            capture.commit_actions,
            vec![kiapi::common::commands::CommitAction::CmaDrop]
        );
    }
}

#[cfg(test)]
mod issue14_chamfer_tests {
    use super::*;

    #[test]
    fn back_side_refresh_mirrors_chamfer_corners() {
        let source = r#"(footprint "F" (layer "F.Cu")
            (pad "1" smd chamfered_rect (at 1 2 30) (size 2 1)
                (layers "F.Cu") (chamfer_ratio 0.2) (chamfer top_left)))"#;
        let pad = super::super::pcb_components::extract_pad_definitions(source)
            .unwrap()
            .remove(0);
        let mirrored = mirror_pad(&pad, 2).unwrap();
        let item = konnect_ipc::KiCadIpcClient::build_footprint_item(
            "L:F",
            "U1",
            "F",
            &[mirrored],
            &[],
            &Default::default(),
            0.0,
            0.0,
            0.0,
            "B.Cu",
        )
        .unwrap();
        let footprint =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        let item = &footprint.definition.unwrap().items[0];
        let pad = kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap();
        let copper = &pad.pad_stack.unwrap().copper_layers[0];
        assert_eq!(copper.chamfer_ratio, 0.2);
        let corners = copper.chamfered_corners.as_ref().unwrap();
        assert!(corners.bottom_left);
        assert!(!corners.top_left && !corners.top_right && !corners.bottom_right);
    }
}
