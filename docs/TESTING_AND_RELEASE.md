# Testing And Release

The pull-request baseline is defined in `CONTRIBUTING.md`. Run the same commands
locally when the required platform dependencies are available:

```bash
cargo test --workspace --locked --lib --tests
cargo test --workspace --locked --doc
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo fmt --all -- --check
```

`protoc` and its well-known type includes are required by
`crates/konnect-ipc/build.rs`.

## v0.14.0 file-safety acceptance map

The milestone implements these contracts together; version stamping, merge and
release are separate operations.

| Issue | Contract and regression evidence |
|---|---|
| #3 | Same-session panic recovery, ping during slow tools, queued/active cancellation, bounded input fairness, and fake-CLI child termination (`stdio` unit tests and `stdio_cancellation`) |
| #5 | Bus geometry snaps to 1.27 mm; resolved physical pin endpoints stay exact; all writes use source revisions (`sch_bus`) |
| #11 | Shared quoted-string encoding; library/graphic/net readback with quotes and backslashes; canonical board-layer validation before mutation |
| #12 | Design writes use revision checks, new assets refuse clobber, duplicate symbol names refuse; final pre-rename interleaving tests and unchecked-writer inventory ratchet |
| #19 | Direct-child coordinate/effects edits preserve quoted values and move global-label fields; committed label readback |
| #20 | Tab-indented library deletion and segment/arc/via routing copies use parser spans and fresh UUIDs |
| #22 | Saved-net route preflight, explicit `allow_join`, UUID disambiguation, local-only junction insertion, malformed-source refusal and whole-batch no-write controls |
| #27 | Every project writer refuses an existing or unreadable project lock; served-dispatch no-write test |
| #28 | Private-field/unknown-child preservation, canonical symbol attributes, byte-preserving no-op and independent serialized-model fixture guards |
| #29 | Attribute merge preserves mask-bridge/future flags; explicit replacement and before/after readback |

For #27, the requested live/stale distinction is unavailable: KiCad 10 lock
files carry a username and hostname without a trustworthy process identity.
The user explicitly chose refusal for **all** remaining locks, including
presumed stale locks; manual removal requires confirming no editor owns the
project. Automatic stale-lock bypass is intentionally excluded.

Runtime tests use temporary files/fake processes where appropriate. Ignored
real-KiCad tests and platform-specific hosted checks are separate evidence;
their absence never counts as a local pass. Coverage must be reported with
its command and measured scope, including unavailable branch instrumentation.

## v0.15.0 PCB fidelity acceptance map

The milestone's implementation and its acceptance tests cover these contracts.
Version stamping and release remain separate operations.

| Issue | Contract and regression evidence |
|---|---|
| #14 | Actual closed-file placement compared with the live handler's captured IPC payload; chamfer geometry, named unsupported-clause refusal before mutation, and stock ESP32/NetTie preservation/refusal (`pcb_components` and `tests/fixtures/placement_fidelity`) |
| #15 | Root-prefixed/root-relative symbol identity, ambiguous and foreign paths, unchanged footprint identity and fresh readback (`pcb_sync`, `ISSUE_15_SYNC_IDENTITY.md`) |
| #16 | Copper zones affect only their named nets; rule areas do not count as routed copper; unavailable net evidence remains a refusal (`PCB_ZONE_NETS.md` and shared routing-snapshot tests) |
| #17 | Shared 4/6/8-layer mirroring, native layer-to-geometry association, mirrored text and chamfer readback, and rollback/uncertain outcomes (`implementation/issue-17-inner-layer-mirroring.md`) |
| #18 | Decimal-grid nanometre conversion, finite/range guards, and actual saved label/text coordinates without floating-point noise (`COORDINATE_PRECISION.md`, `coordinate_precision_test`) |
| #25 | Library attributes, all placed symbol units, flag queries, BOM-excluded board placement, and strict final identity/attribute readback (`issue25` tests and `native_assembly_attributes_survive_real_kicad_bom_export`) |
| #26 | Owned schematic fields versus board-only fields, LCSC update then noop, grouped-footprint refusal before commit, library field presentation, dashed strokes, and reset-text behavior (`pcb_sync`, `pcb_footprint_update`) |

Live-handler tests above use mock IPC and saved-file observations. They do not
claim native editor execution. The real KiCad workflow must execute the ignored
CLI acceptance tests at the candidate head, including the native DNP/BOM and
variant tests; a skipped CLI lookup is not execution evidence. Record the exact
head, four normal Rust gates, measured line/branch coverage, hosted checks, and
any remaining host-specific limits in the terminal PR.

### KiCad 10.0.8 follow-up acceptance

- L02 observes which item classes the exact board's IPC session can list.
  A successful empty tables list and an explicit refusal of generators are
  distinct responses. Keep the comparison with saved board bounds.
- L06 and L07 require uniquely placed stock footprints and saved geometry.
  L09 independently verifies library refresh through apply, convergence and
  saved readback.
- L08 and the full geometry workflow verify a fresh PCB sync through
  `ready → applied → noop`. Sync and library refresh compare the published
  board after the single KiCad commit. A post-commit verification failure
  reports the acknowledged commit and prevents automatic retries.
- Run the variant CLI test under the English and Japanese locales through the
  real BOM export. The wrong-value and population controls must fail semantic
  comparison in both locales.
- For a manufacturing workflow, compare the two nets in the schematic
  netlist and placed pads, inspect the routed tracks, and require zero DRC
  errors, warnings, unconnected items and schematic differences. Validate
  the Gerber layers and drill positions; an unrouted control must stop before
  export.

## Measuring Coverage

Workspace line and branch coverage (stable toolchain needs `RUSTC_BOOTSTRAP=1`
for branch instrumentation, which is otherwise unavailable):

```sh
RUSTC_BOOTSTRAP=1 cargo llvm-cov --workspace --locked --lib --tests --branch
```

Use a private `CARGO_TARGET_DIR` when other builds share the checkout. The
viewer crate and ignored real-KiCad tests are outside this measurement.

Umask probe tests (`konnect-sexp` `mode_probe::run_under_umask`) re-run the
test binary under `umask 277` or `umask 777`. Under coverage that child writes
its `.profraw` with the umask applied, which can leave a mode-000 file, and
`llvm-profdata merge` then dies with a SIGTRAP (`InstrProfError::take`) after
every test has passed. The probe now points the child's `LLVM_PROFILE_FILE` at
a scratch directory, so no unreadable profile reaches the merge. If the
SIGTRAP returns, look for `----------` entries in
`target/llvm-cov-target/*.profraw` and trace which test spawns a child process.

## Coverage Map

| Area | Source of tests |
|---|---|
| MCP protocol, CLI, and asset contracts | `crates/konnect/tests` and `crates/konnect/src` unit tests |
| Router and toolset invariants | `crates/konnect-core/src/router` tests |
| Dispatch and required-argument behavior | `crates/konnect-core/src/mcp/handler.rs` tests |
| Domain handlers and evidence rules | tests beside modules in `crates/konnect-core/src/tools` |
| S-expression parsing and atomic writes | `crates/konnect-sexp` tests |
| Typed schematic model | `crates/konnect-schematic-editor` tests |
| IPC builders, transport, and client behavior | `crates/konnect-ipc` tests |
| Real KiCad behavior | ignored live tests and `.github/workflows/e2e-kicad.yml` |
| Viewer | `crates/schematic-viewer` tests and its CI job |
| Python plugin | `plugin/tests` and the plugin CI job |
| PCM package | `packaging/validate-pcm.py` and packaging CI jobs |

The viewer is outside the Cargo workspace. Build or test it from
`crates/schematic-viewer`; workspace commands do not cover it.

## Evidence-Focused Regression Tests

When a tool returns a count, success state, or verdict, test the evidence behind
that field. The v0.7 reference cases include:

- complete DRC category parsing in `tools/cli.rs`;
- DRC-backed review/readiness decisions in `tools/design_review.rs` and
  `tools/manufacturing.rs`;
- footprint-type discrimination and post-commit read-back in
  `tools/pcb_sync.rs` and `konnect-ipc/src/builders.rs`;
- closed-board placement and flip refusal cases in `tools/pcb_components.rs`.

Use real KiCad-generated fixtures for formats KiCad owns. For an IPC path, unit
tests should prove request construction and failure classification; an ignored
live test or the end-to-end workflow should prove behavior that depends on a
running editor.

IPC doubles in `konnect-core` must use the shared `test_support::MockIpcServer`.
The returned guard owns an already-listening, process-unique `inproc://`
endpoint and joins its worker when dropped. Retain that guard for the whole
test. Do not discover a free TCP port by binding it, dropping the listener, and
then asking NNG to bind the same port: another process can claim the port in
between, making otherwise deterministic tests fail with `AddressInUse`.

The Specctra import undo boundary has a manual live gate because KiCad IPC can
create a named commit but cannot invoke the editor's Undo action. Open a
disposable copy of
`crates/konnect-core/tests/fixtures/specctra_two_resistors_locked.kicad_pcb`,
set `KONNECT_LIVE_SPECCTRA_BOARD`, `KICAD_API_SOCKET`, and (when it is not on
`PATH`) `KICAD_CLI_PATH`, set `FREEROUTING_JAR` to the local Freerouting 2.3.0
JAR, then run:

```text
cargo test -p konnect-core --locked one_undo_restores_the_exact_pre_import_board_snapshot -- --ignored --nocapture
```

When the test prints `LIVE_UNDO_READY`, press Ctrl+Z once in PCB Editor. The
test passes only when the exact pre-import IPC snapshot returns.

## CI And Live Validation

For the CLI and live-GUI evaluation checklist, commands, acceptance criteria,
and evidence template, see the [KiCad E2E runbook](KICAD_E2E_RUNBOOK.md).

`.github/workflows/ci.yml` covers the Rust workspace, formatting, clippy,
documentation tests, viewer, plugin, Nix, and PCM validation. The real-KiCad
workflow remains separate because it installs KiCad and is not an ordinary
per-PR gate. Apply the `run:e2e-kicad` label to run that same workflow for a
pull request when KiCad-facing behavior needs hosted acceptance evidence.

Before creating a release tag, dispatch `.github/workflows/release.yml` against
the exact candidate ref. The manual entry point runs the release-profile target
matrix, all three PCM package builds, and the reusable real-KiCad acceptance
workflow with a synthetic `0.0.0` package version. Its publication job is
tag-only, so this pre-tag smoke run cannot create a GitHub release.

On a release tag, `.github/workflows/release.yml` calls
`.github/workflows/e2e-kicad.yml` as a reusable workflow. The called workflow
checks out the caller's exact tag commit. The `Create Release` job depends on
that acceptance job and both artifact-building jobs, so failed real-KiCad
acceptance leaves the diagnostic build artifacts in the workflow run but cannot
create a GitHub release or upload a partial release asset set. Weekly and manual
real-KiCad runs continue to use the standalone workflow entry points.

In the PR description, list every command run and explicitly name checks skipped
because they require KiCad, another operating system, credentials, or release
infrastructure.

## Documentation

For a public behavior change, inspect README, `DEV.md`, `tool-directory.md`,
these developer maps, and bundled guidance under
`crates/konnect/assets/skills` and `assets/agents`. Tool-count locations and
their enforcement are defined by `CONTRIBUTING.md` and
`crates/konnect/tests/doc_tool_counts.rs`; link to the authoritative catalogue
instead of copying totals into new documents.

Every behavioral claim in these maps should name its source module. When the
module changes, a contributor can find the claim by searching for the old path.
Describe unimplemented future behavior explicitly as design intent rather than
current capability.

### Linux release compatibility and dependency gates

Linux release binaries target glibc 2.35. The release build uses Ubuntu 22.04
for x64 and arm64 and runs `packaging/check-glibc.sh` on the server binaries and
the x64 PCM server and viewer before packaging. The Linux compatibility jobs
install the candidate npm packages on Ubuntu 22.04 and Debian 12 on both
architectures and exercise both `--version` and MCP `initialize`.
The tag workflow gates publication on those jobs; the manual dispatch exercises
the same builds and compatibility checks without publishing.

`Published npm acceptance` checks the public registry with a clean npm cache on
macOS ARM64, Windows x64, and Linux ARM64. Dispatch it with an exact published
version, or run `node npm/scripts/smoke.js 0.16.0 --published` on a supported
host. The smoke installs with scripts disabled, selects the native binary via
the package launcher, and checks `--version`, MCP initialization, tool listing,
and installation info. It exercises the transport used by desktop clients.
Client-specific configuration, UI, and permission prompts require separate
checks in the actual client.

The weekly CI dependency checks cover the root workspace and the separate
viewer lockfile. To reproduce both gates locally:

```sh
cargo deny --locked --all-features check advisories licenses
cargo deny --locked --all-features --manifest-path crates/schematic-viewer/Cargo.toml check advisories licenses
```

## Packaging And Release

Build the server with `cargo build --release -p konnect`. Build the viewer
separately when it is in scope. Changes to plugin files, binary layout, metadata,
icons, or release scripts require PCM assembly and
`packaging/validate-pcm.py` validation.

`packaging/build-pcm.ps1` and `build-pcm.sh` enumerate the files staged into the
PCM archive. Developer documentation stays in the repository and is not added
to release zips.

### Interrupted project rename recovery

`rename_project` checks original and destination editor locks plus child-sheet
locks before renaming. If a lock, read error, or revision conflict is observed
after filenames have changed, it returns `mutation_outcome_uncertain` with
`renamed_pairs`, completed `content_rewrites`, `failing_target`, and any
`rollback_failures`. Inspect that inventory and preserve external changes before
reconciling filenames and project references. Never blindly retry the rename or
roll back already rewritten contents. Project names are escaped independently
for JSON metadata and S-expression annotation keys.

`rename_project` uses the shared native exclusive rename primitive for forward
moves, rollback, backup directories and optional project-directory moves. A
new destination appearing after planning is refused atomically. Unsupported
platform/filesystem operations fail closed rather than falling back to a
replacing rename or a copy/delete sequence. Regression tests inject both a
forward collision and a newly recreated original during rollback.
