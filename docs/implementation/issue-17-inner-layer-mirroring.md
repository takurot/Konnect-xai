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
that readback. Pad signatures retain position, type, rotation and geometry
sizes, so same-number pads cannot exchange layer assignments unnoticed. Child
order and generated IDs do not affect comparison. Repeated children retain their
multiplicity. Graphics retain their geometry association, while text retains its
position and content association.

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
