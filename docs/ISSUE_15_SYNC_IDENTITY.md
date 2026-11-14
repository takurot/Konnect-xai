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

## Verify the complete live identity before publishing

The sync records the exact `SheetPath` sent for each change. Updates are matched
by their existing KIID, so reannotation cannot redirect verification to another
footprint with the requested reference. Additions are matched by their unique
new reference because KiCad may assign their KIID. Both checks require the
requested reference and the complete UUID and human-readable path.

The requested document is read inside the recovering transaction, after the
mutations and before publication. Missing, duplicate, malformed or altered
identity evidence aborts that transaction. Ordinary readback failures permit the
existing rollback. Unknown transport outcomes retain their original recovery
handling. A readback mismatch is returned as an uncertain result after the
rollback attempt, rather than an `applied` response or a claim that no write
was attempted. Inspect the live board before retrying.

An extracted execution of the original readback function reproduces acceptance
of a missing updated footprint. This harness stubs the client and footprint
shape tally, so it is narrower than the registered tool. The new exact identity
checker passes three isolated tests using the real KiCad capture and protobuf
types. Neutralizing only the complete-path comparison makes its altered-path
regression fail. These checks do not prove live KiCad behavior.

The committed registered-tool test injects changed UUIDs, changed human-readable
paths, a stale reference, a missing path, a missing footprint and malformed
protobuf after updating. It requires a non-success response, a dropped undo
commit and restored mock board state. The normal case still verifies preserved
board-only objects and a following no-op plan. Its Cargo execution and the
workspace gates remain pending in the shared verification lane.
