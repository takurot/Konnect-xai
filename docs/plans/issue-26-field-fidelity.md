# Preserve footprint fields and library presentation

This change implements [Issue #26](https://github.com/takurot/Konnect-xai/issues/26)
after Issue #14's shared live-footprint validator. Existing target checks,
reviewed plan revisions, recovery, and readback remain required.

## Synchronize schematic fields

`update_pcb_from_schematic` copies exported custom fields, Datasheet, and
Description on additions and updates. Reference and Value retain their native
fields. Footprint selects the library and is not duplicated as custom metadata.
Duplicate names and the reserved ownership name refuse the operation.

The hidden footprint property `Konnect:schematic_fields` records a versioned
list of names and values last supplied by the schematic. Initial synchronization
preserves fields of unknown origin. Later synchronization removes a missing
schematic field only if its current value still matches the recorded value.
A field edited only on the board is preserved when the schematic drops it.
Other board-only fields remain unchanged. Malformed, oversized, duplicate, or
unsupported ownership records refuse synchronization before mutation.

Ownership and field values affect the reviewed plan revision. Independent live
readback checks the values that were sent. A mismatch reports an uncertain
outcome and requires inspection before another mutation.

## Preserve authored fields and strokes

Placement, schematic additions, and library refresh use one shared library
property parser. Native fields retain their authored layer, position, font
size, thickness, line spacing, visibility, and upright behavior. Custom library
properties travel as typed fields and are not duplicated as drawing text.
Legacy `fp_text reference` and `fp_text value` use the same parser.

Graphic stroke types preserve `solid`, `default`, `dash`, `dot`, `dash_dot`,
and `dash_dot_dot`. New ordinary graphics explicitly send the native solid
enum. Equivalent default/solid graphics can remain a no-op, while dashed
courtyards retain their style across a refresh.

`update_footprints_from_library` accepts `reset_text`, a boolean defaulting to
`false`. When true, Reference and Value restore the library's presentation
while retaining their placed text. This choice affects the plan revision.
The next unchanged refresh returns no-op.

## Refuse lossy live updates

Before preparing an update, the exact live document is serialized through
`SaveDocumentToString`. Group membership, footprint groups, points, and variant
state that the bundled IPC model cannot preserve produce a named conflict for
the affected footprint. Saved disk contents do not substitute for unsaved live
state. Unrelated footprints and no-op selections do not cause a refusal.

## Validation status

The unchanged native stroke builder was executed in a pinned-rustc harness.
It returned unknown style `0` where explicit solid `2` was required. This is
extracted-source reproduction, not a real KiCad session.

Regression tests cover field ownership, first-sync preservation, corruption,
roundtrip values, live loss findings, field presentation, all native stroke
styles, reset-text convergence, and revision changes. Full workspace gates,
coverage, and real KiCad acceptance remain pending until recorded here.
No KiCad installation is available on this host. No real IPC acceptance or
80-percent coverage claim is made.
