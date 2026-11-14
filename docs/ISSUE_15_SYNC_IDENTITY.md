# Match schematic identities across root path spellings

`update_pcb_from_schematic` must match a symbol whose board path includes the
requested schematic's root UUID to the exported root-relative path. Matching
must also survive reannotation without adding a second footprint.

The saved root UUID and actual sheet instance paths define the identity context.
Only that exact root prefix is removed for matching. A path outside that saved
hierarchy, or two board footprints with the same normalized identity, refuses
the whole plan before mutation. Root-relative paths remain valid.

Matched footprints keep their stored path spelling. New additions keep the
exported spelling. An equivalent spelling alone does not require an update.
Skipped and unassigned components use the same matching keys. Existing target
validation, transaction recovery, pose preservation, revision checks, and
readback remain in the sync path.

This change does not alter component attributes, pad geometry, zone routing,
library refresh, or schematic export.

## Acceptance checks

- Root-relative and exact-root-prefixed paths match at the root and in sheets.
- Reannotation updates the existing footprint instead of adding another one.
- Stored matched paths survive updates and do not cause repeated updates.
- Normalized duplicates and foreign roots refuse the plan before mutation.
- Skipped and unassigned identities match after reannotation.
- Plan revisions distinguish raw stored paths and requested identity contexts.
- IPC readback confirms the stored path survives the update.

## Validation record

The original planner was compiled in an extracted-source Rust harness. Both
regressions failed for the expected reasons. Root-prefixed matching returned
`reference_identity_conflict`, and reannotation planned a duplicate addition.
The updated planner passes five harness checks for matching, reannotation,
no-op behavior, normalized collisions, and foreign roots. Disabling the foreign
root guard makes its regression fail. Revision hashing is stubbed in that
harness, and saved-hierarchy loading is not exercised there.

The committed Rust tests also cover complete stored `SheetPath` preservation,
root-bound plan revisions, skipped and unassigned matching, and a registered
tool update with independent mock IPC readback followed by a no-op dry run.
Their execution is pending the shared Rust verification lane.

Pinned `rustfmt` and `git diff --check` pass. The complete workspace test,
doctest, Clippy, and line and branch coverage gates are pending. Real KiCad is
unavailable on this host. Mock IPC readback does not establish live KiCad
behavior. This implementation is not a completed validation result.
