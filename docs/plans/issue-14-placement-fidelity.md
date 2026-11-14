# Preserve footprint geometry during live placement

Issue #14 requires live placement to preserve supported library geometry and refuse unsupported clauses before writing. File placement remains lossless and can accept library clauses that the typed IPC representation cannot carry.

## Design

The prepared placement owns the original board S-expression and a separate result for its typed IPC representation. An unsupported typed clause cannot prevent the guarded file path when KiCad is unreachable. Once KiCad answers, a typed refusal never permits a file fallback.

The existing library-refresh validator moves into the shared library-footprint module. Single placement, footprint arrays, schematic synchronization and library refresh use the same supported subset. Diagnostics identify the footprint, reference and clause. The typed pad model adds chamfer ratio and four named corners. IPC builders reject unknown pad shapes. Native and saved pad reads expose the same corner representation. Back-side refresh mirrors the top and bottom corner selections with the pad geometry.

The change does not add support for custom pad primitives, footprint zones, net ties, margins, thermal overrides or other refused clauses. A refusal points callers to closed-board file placement. Field presentation and schematic custom-field synchronization remain in Issue #26.

## Acceptance checks

- Every unsupported category named in Issue #14 either survives the typed payload or produces a named refusal before mutation.
- The closed-board path preserves the same original library clauses.
- Chamfer ratio and corner selection survive extraction, payload building and native pad decoding.
- Back-side refresh mirrors corner selection without changing the ratio.
- Unknown pad shapes never become rectangles.
- Sync reports unusable libraries through its existing per-reference diagnostics.

## Verification record

The unchanged `extract_pad_definitions` function and its exact original `IpcPadDefinition` data shape were extracted into a pinned-rustc harness linked to the real `konnect-sexp` and `anyhow` build artifacts. The original parser failed the assertion `unsupported live pad shape was accepted` with exit 101. The corresponding current parser passes unknown-shape refusal, chamfer ratio and corner retention, invalid-ratio refusal and unknown-corner refusal.

This extracted-source harness proves parser behavior only. It does not prove served-handler behavior, native IPC payload acceptance or KiCad editor behavior. Full handler and native decode regressions are checked into the source and await the serialized Rust verification lane. The full workspace gates and line and branch coverage measurements are pending.

Real KiCad is unavailable on this host, so native-editor verification remains unconfirmed.
