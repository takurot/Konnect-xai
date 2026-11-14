# Preserve native symbol attributes

Issue #25 separates board membership from BOM and position-file membership. A mounting hole excluded from the BOM still belongs on the board. DNP is a native flag rather than a custom text property.

The change extends the existing library metadata and component mutation paths. A typed attribute set represents effective values. Optional patches distinguish omitted parameters from explicit false. New placements inherit library attributes before applying overrides. The specialized power-symbol placer also copies the same library attribute set so its bound placement intent matches the saved instance. Existing edits change only the requested native clauses on every placed unit. Queries and saved-file readback report effective native attributes.

PCB synchronization honors `on_board` for placement and carries DNP, BOM exclusion, and position exclusion independently. Its plan, revision identity, and readback cover these values. Native assembly readback requires the exact footprint message type, decodable payloads, nonempty unique KIIDs, and an unambiguous expected reference. Updates must retain the planned KIID; additions must receive a server-assigned KIID. A native assembly mismatch inside the transaction attempts rollback and remains uncertain until restoration can be independently verified. Any readback failure after publication returns an uncertain outcome and forbids a blind retry. Components omitted through `on_board=no` remain visible with a reason. Components without an assigned footprint remain in the existing per-component unassigned report.

## Throughput checkpoint

- Blocking first steps. The milestone specification passed independent review before implementation. Reproduction tests run before implementation.
- Independent workstreams. This Issue uses its own worktree and branch. It shares no writable checkout with the other milestone fixes.
- Shared mutable state. Rust checks use the root's serialized verification lane because this host has 8 GiB RAM.
- Smallest safe decomposition. One owner extends the schematic and synchronization consumers together so they use the same native semantics.

## Acceptance checks

- Mounting-hole placement inherits library exclusions and supports explicit overrides.
- Single and batch native edits reach every placed unit and survive saved-file readback.
- Invalid native booleans and misleading custom DNP properties refuse before mutation.
- An excluded BOM component remains eligible for PCB placement.
- Sync adds and updates the native DNP, BOM, and position attributes and notices their changes in plan revisions.
- Wrong native flags, replaced update KIIDs, duplicate references or KIIDs, malformed or mistyped messages, and rejected final readback never report applied success.
- The ignored native CLI acceptance test exports a Reference-only BOM before and after a served MCP edit of all three U1 units in the KiCad-authored ECC83 fixture. It requires U1 to disappear under `--exclude-dnp` while R1 remains. It also places the installed `Mechanical:MountingHole` symbol and checks the embedded library and saved instance retain `in_bom=no` and `on_board=yes`. Native execution remains unverified until the KiCad E2E workflow runs at the new head.

## Verification record

The original `apply_saved_symbol_flags` function was extracted without changes and run with the pinned compiler and the actual S-expression parser. An `in_bom=no`, `on_board=yes` mounting-hole instance produced zero design components instead of one. The assertion failed with exit code 101. The updated function passed the same check.

A pinned-compiler check also passed library attribute inheritance and native symbol serialization readback. A second extracted-source check passed multi-unit attribute edits against the KiCad-authored `ecc83_multiunit.kicad_sch` fixture, explicit false, omitted attributes, and malformed boolean refusal. These are function-level checks, not a substitute for the complete served tool or real KiCad.

The first complete `cargo test -p konnect-core --locked --lib issue25 -- --nocapture` build found two placement test constructors that omitted the new attribute model. Both constructors now bind the fixture symbol's attributes. Subsequent shared-target runs remain diagnostic evidence only because the shared build target could reuse workspace artifacts from another worktree. An isolated-target run subsequently passed the native tests, served query tests, all four Rust gates, and an assembly-guard negative control with an exact source restoration. Independent review then identified final-readback identity and error-classification gaps. Four new served regressions failed for the intended reason before the strict readback fix; malformed-payload readback was already uncertain. The final code requires a fresh isolated-target run of the focused tests and all four gates. Coverage was interrupted for these functional fixes, so no line or branch percentage is claimed. Real KiCad export and IPC checks are unavailable on this host.

The `deslop` skill is unavailable in this installation. The implementation receives a manual scoped diff cleanup and independent review instead.

The native BOM acceptance test pins `--group-by Reference` and a quote delimiter so its single-reference row comparisons do not depend on grouping presets. These options and `--exclude-dnp` are supported by [KiCad 10.0.5’s BOM command](https://github.com/KiCad/kicad/blob/10.0.5/kicad/cli/command_sch_export_bom.cpp). A missing CLI fails an explicitly requested E2E run. Normal Rust gates only compile this ignored test and cannot establish the native acceptance result.
