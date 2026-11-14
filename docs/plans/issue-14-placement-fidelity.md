# Preserve footprint geometry during live placement

Issue #14 requires live placement to preserve supported library geometry and refuse unsupported clauses before writing. File placement remains lossless and can accept library clauses that the typed IPC representation cannot carry.

## Design

The prepared placement owns the original board S-expression and a separate result for its typed IPC representation. An unsupported typed clause cannot prevent the guarded file path when KiCad is unreachable. Once KiCad answers, a typed refusal never permits a file fallback.

The existing library-refresh validator moves into the shared library-footprint module. Single placement, footprint arrays, schematic synchronization and library refresh use the same supported subset. Diagnostics identify the footprint, reference and clause. The typed pad model adds chamfer ratio and four named corners. IPC builders reject unknown pad shapes. Native and saved pad reads expose the same corner representation. Back-side refresh mirrors the top and bottom corner selections with the pad geometry.

Placement, arrays, schematic sync and library refresh compare native chamfer geometry after writing. The comparison binds each chamfer to the transmitted pad coordinates, normalized angle, layer set, per-layer shape, size, offset and corner-rounding ratio. Repeated pad numbers and reordered child messages remain valid. Existing footprint KIIDs must stay unchanged. New placements accept KiCad-assigned KIIDs. A placement or sync readback mismatch returns an uncertain outcome that requires inspection before retrying.

Library refresh verifies its chamfers inside the existing commit. A known readback failure drops the commit and returns an error that explicitly states rollback. A native unknown outcome or failed rollback retains its structured uncertain result. These results never become a preflight conflict or an applied result.

The change does not add support for custom pad primitives, footprint zones, net ties, margins, thermal overrides or other refused clauses. A refusal points callers to closed-board file placement. Field presentation and schematic custom-field synchronization remain in Issue #26.

## Acceptance checks

- Every unsupported category named in Issue #14 either survives the typed payload or produces a named refusal before mutation.
- The closed-board path preserves the same original library clauses.
- Chamfer ratio and corner selection survive extraction, payload building and native pad decoding.
- Back-side refresh mirrors corner selection without changing the ratio.
- Simulated corner loss after an accepted write returns an uncertain outcome. Reordered pads with repeated numbers preserve the same multiset and pass.
- Exchanging corner selections between same-number pads at different positions fails readback. Changed layers, size, offsets and angles also fail.
- Back-side refresh tests faithful, missing, malformed, unavailable and dropped-corner readback. Confirmed rollback restores the original mock state. A failed rollback reports an unknown outcome.
- The same unsupported-clause matrix exercises live refusal and closed-file preservation for footprint children and pad thermal, clearance, mask, paste, property and drill-offset overrides.
- Unknown pad shapes never become rectangles.
- Sync reports unusable libraries through its existing per-reference diagnostics.

## Verification record

The unchanged `extract_pad_definitions` function and its exact original `IpcPadDefinition` data shape were extracted into a pinned-rustc harness linked to the real `konnect-sexp` and `anyhow` build artifacts. The original parser failed the assertion `unsupported live pad shape was accepted` with exit 101. The corresponding current parser passes unknown-shape refusal, chamfer ratio and corner retention, invalid-ratio refusal and unknown-corner refusal.

This extracted-source harness proves parser behavior only. It does not prove served-handler behavior, native IPC payload acceptance or KiCad editor behavior. Full handler and native decode regressions are checked into the source and await the serialized Rust verification lane. The full workspace gates and line and branch coverage measurements are pending.

Real KiCad is unavailable on this host, so native-editor verification remains unconfirmed.

## Independent review corrections

The original chamfer multiset accepted swapped corner selections on physically different pads sharing a number. A new regression failed against the actual original `chamfer.rs` module. Binding the geometry to its physical pad makes that regression pass while retaining valid reordering.

The original refresh commit block also accepted an update response without observing its chamfers. An extracted original commit-block harness failed the dropped-corner predicate. The current exact callback and chamfer module pass seven isolated checks, including confirmed drop on mismatched or missing readback. Neutralizing only the position binding makes the new corner-swap regression fail again. These harnesses use compiled protobuf types and builders with a reference-accessor shim or a stub commit client. They do not replace the full served IPC regressions or the workspace gates, which remain pending in the serialized verification lane.

## Coincident pad geometry review correction

Chamfer signatures include the supported corner-rounding ratio. Two pads may
share a number, position, layer, size and angle while using different rounded
corners. Exchanging their chamfer masks must fail, while reordering intact pads
remains valid. Non-finite rounding ratios are refused.

The isolated-target Cargo regression failed before the correction, with one
failed assertion that the exchanged masks must be rejected. The exact current
module harness passes that case after the correction. Neutralizing the rounding
association in a temporary module makes the same regression fail again. The
harness uses vendored protocol types and does not prove the complete branch.
All previous shared-target Cargo results are superseded. Final isolated-target
quality gates, coverage and real KiCad verification remain pending.

The served two-unusable-footprints regression retains its exact diagnostic
objects, both footprint IDs and all three affected references. Its golden text
now matches the shared validator's named custom-pad refusal and closed-file
recovery guidance. The previous string-only failure was observed in the
isolated descendant suite. Parent-branch rerun remains part of the final gates.
