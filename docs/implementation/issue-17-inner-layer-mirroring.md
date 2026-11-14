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
