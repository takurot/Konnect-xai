# Mirror footprint layers against the board copper stack

Issue #17 makes closed-board footprint flips and back-side library refresh use
KiCad's copper-stack mapping. On eight copper layers, `In2.Cu` becomes `In5.Cu`.
A second flip restores the original layer.

## Scope and design

`konnect_ipc::builders::flip_layer` owns layer mirroring. The function accepts a
layer name and the board copper-layer count. It resolves front/back aliases with
the existing IPC layer map, preserves wildcard layer sets, and refuses inner
layers outside the actual stack. Counts outside KiCad's supported even range
cannot establish an inner-layer mapping.

Closed-board flips read the copper count from the saved board's layer table.
Library refresh reads the count for the exact live document through
`GetBoardEnabledLayers`. The refresh plan revision includes that count, so a
reviewed plan cannot survive a stack change. Pad, graphic, and custom-property
layers all use the shared function. Numeric IPC field layers use the existing
name conversion around the same function.

The alternative was another layer table in the S-expression crate. Keeping the
mapping beside the IPC name and enum conversion avoids a third alias table.
Existing unsupported closed-board geometry remains a refusal. The change does
not add zone, custom-pad, or 3D-model transformations.

## Acceptance and tests

- Table cases cover four, six, and eight copper layers, both directions, and
  outer-layer pairs, aliases, wildcard sets, and non-side layers.
- Invalid inner indices, absent layers, and unknown layers are refused.
- An eight-layer closed-board fixture mirrors inner graphics, pads, and field
  layers. A second flip restores them.
- A back-side refresh mirrors inner pad, graphic, and numeric custom-field
  layers. An unavailable inner layer refuses the rebuild.
- Changing the live copper count changes the reviewed plan revision.

## Validation record

Specification reviewed independently before implementation. Runtime results are
recorded after the focused tests and repository quality gates complete.

Real KiCad IPC and GUI validation requires a KiCad installation. None was found
in this environment. Mock payloads do not establish real KiCad readback.

The first executable reproduction extracted the two existing helper bodies from
`HEAD` and compiled them with pinned `rustc 1.96.0`. Both assertions failed with
actual `In2.Cu` and expected `In5.Cu`. This proves the helper defect, not the
complete tool path. The pending full focused tests cover both consumers.

## Independent review corrections

Back-side library refresh treats all copper layers as side-specific for text
mirroring, including inner copper. The rebuild test includes `fp_text` on
`In2.Cu` and requires both `In5.Cu` and mirrored glyphs after refresh. This follows
[KiCad's side-specific layer mask](https://docs.kicad.org/doxygen/lset_8cpp_source.html)
and [text flip implementation](https://docs.kicad.org/doxygen/pcb__text_8cpp_source.html).

Before publishing the update commit, refresh reads the exact live document's
footprints again. It compares the planned layer and mirrored-text state against
that readback. Pad signatures retain normalized supported wire geometry, including corner
rounding and drill dimensions, so coincident same-number pads cannot exchange
layer assignments unnoticed. Child
order and generated IDs do not affect comparison. Repeated children retain their
multiplicity. Graphics retain stroke geometry and text retains its authored font, angle,
size, alignment and effects. Assigned child IDs, read-only parents and full-turn
angle representations do not change those associations.

Known readback mismatch or unavailable evidence fails inside the transaction and
requests its normal rollback. A failed rollback remains `ipc_outcome_unknown`;
the handler does not convert that typed uncertainty into a planning conflict.
Native transport uncertainty keeps the existing no-blind-rollback behavior.

The review reproduction compiled the original side-specific helper and original
mutation callback with pinned Rust and actual IPC types. Both requested
assertions failed. The callback harness uses a stub client, so it proves the
missing observation rather than native KiCad behavior. Extracted current
production helpers pass three checks for copper-text mirroring, wrong/missing
readback, and duplicate-pad multiplicity. Full Cargo handler, rollback, physical
pad-association tests, coverage and native-editor checks remain pending at this
checkpoint. The added native mock tests require normal commit on matching
readback and rollback on layer, mirror, missing-footprint and unavailable
readback failures.

A negative control disabled only the layer-readback equality in the extracted
harness. Wrong-layer and duplicate-pad checks then both failed. The committed
guard remains enabled. Full pending workspace results must be rerun against this
checkpoint because an earlier build may have read the previous source.

The first full workspace test run reached the core suite and reported 1,775
passed, three failed and 20 ignored tests. The three new layer-readback tests
used the older planning fixture, whose pads intentionally omit padstack data.
The strict observation checker refused that incomplete evidence. Those tests
now rebuild the existing KiCad-authored socket fixture to obtain complete typed
pads. The inner-layer loss test explicitly starts on `In2.Cu` before injecting
`In5.Cu`. A separate test requires missing padstack evidence to remain a refusal.
Production validation is unchanged. The targeted `layer_readback_` rerun is
queued; its result and the final-head full gates remain pending.

## Coincident geometry review correction

Readback records use normalized supported pad, text and graphic geometry rather
than partial coordinate keys. They preserve the association of each layer with
roundrect rounding, text angle and font effects, or graphic stroke width and
style. Exact Any type checks precede typed decoding and canonical repacking.
This prevents foreign message names with matching suffixes from satisfying the
comparison. Repeated records remain a multiset.

Before the correction, the exact current helpers failed three regression cases
for coincident roundrect pads, text and strokes. The corrected helpers pass
those cases. Neutralizing their geometry association in a temporary extracted
harness causes all three cases to fail again. The isolated-target Cargo command
`cargo test -p konnect-core --locked --lib layer_readback_binds_layers_to_` now
passes all three committed regressions. This targeted result does not establish
the full quality gates or native KiCad acceptance. Earlier shared-target Cargo
results are superseded for final validation. Final-head gates and coverage
remain pending.

## Inactive native pad geometry

KiCad's recorded R1 footprint carries a chamfer ratio of 0.2 on ordinary pads.
The vendored `PadStackLayer` protocol declares chamfer ratio and corners active
only for `PSS_CHAMFEREDRECT`, and corner rounding active only for roundrect and
chamferedrect shapes. Comparison now clears these inactive values in the shared
pad normalizer. The transmitted footprint data is unchanged.

The exact current normalizer and layer-readback helpers failed a regression
derived from the real R1 capture before this correction. Changing only inactive
corner values is now equivalent in both comparisons. Four active corner cases
remain unequal. All five extracted helper regressions, including the existing
coincident pad, text and stroke cases, pass. Removing only the inactive-value
normalization makes the native-capture regression fail again.

These helper checks link protobuf types from this worktree's isolated target.
They do not establish the complete crate, workspace gates, coverage or live
KiCad acceptance. Those checks remain required at the final head. Manual scoped
diff cleanup and direct pinned rustfmt were used before this checkpoint.
