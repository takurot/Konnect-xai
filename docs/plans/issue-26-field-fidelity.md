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
Native Datasheet and Description fields clear to empty text when removed.
Other board-only fields remain unchanged. Malformed, oversized, duplicate, or
unsupported ownership records refuse synchronization before mutation.

Ownership and field values affect the reviewed plan revision. A shared live
readback signature checks the native and custom field text, effects, layers,
visibility, and the stroke style attached to each graphic's geometry and layer.
It also checks the footprint's reference, library identity, position, side, and
existing UUID. Assigned child IDs and full-turn angles do not change the
presentation. Known parent links must identify the containing footprint.
Placement, arrays, schematic sync, and library refresh use this same check.
Missing, ambiguous, unreadable, or changed results after publication report an
uncertain outcome and require inspection before another mutation.

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

At the pre-review checkpoint, the six new shared-library regressions passed in
an isolated build of the exact source with pinned Rust. Disabling ownership removal and live loss findings in
temporary source copies makes their two regressions fail. The checkout keeps
both guards enabled. These checks do not verify served IPC behavior.

Regression tests cover field ownership, first-sync preservation, corruption,
roundtrip values, live loss findings, field presentation, all native stroke
styles, reset-text convergence, and revision changes.
Earlier Cargo results used a shared worktree target and are superseded because
that cache could reuse another worktree's workspace artifact. Final verification
must use this worktree's isolated target and exact source. The source harnesses
are supporting evidence only.

No KiCad installation is available on this host. No real IPC acceptance or
80-percent coverage claim is made.

The real R1 protocol capture reproduces a further no-op failure: native pads
carry an inactive chamfer ratio of `0.2`, while the library builder supplies
`0`. Pad comparison now clears only attributes that the protocol declares
inactive for that shape. Roundrect radii and chamfered-rectangle radii, ratios,
and corner selections remain significant. The exact-source harness fails
before this correction and passes afterward; a native-fixture regression also
checks these active-value counterexamples. This regression passes in the final
isolated Cargo run recorded below.


Final functional verification at source commit
`3ff761cc2ebc6080c8dd1111a663d78b2e3358a4` used this worktree's isolated
`target`, with all workspace packages rebuilt under the serialized verifier:

- Targeted presentation, builder, NNG placement and core tests: 21 passed.
- `cargo fmt --all -- --check`: passed.
- `cargo test --workspace --locked --lib --tests`: 2,581 passed, 45 ignored.
- `cargo test --workspace --locked --doc`: 9 passed, 2 ignored.
- `cargo clippy --workspace --locked --all-targets -- -D warnings`: passed.
- Bypassing presentation verification makes its regression fail; restoring the
  source passes all four presentation tests.
- Disabling ownership removal and live loss findings makes two of six metadata
  regressions fail; restoring the source passes all six.
- `git diff --exit-code` after restoration: passed.

The ignored tests remain unverified. The report and individual logs are in
`/tmp/konnect-v015/issue26-final/`; the serialized batch exited zero. Line and
branch coverage for this source are still unmeasured. No 80-percent claim is
made. Independent review of the final source and real KiCad acceptance remain
required before treating the issue as complete. Manual cleanup covered only the
changed code because the deslop tool is unavailable.
