# Developer Overview

This is the map layer for developers who need to understand Konnect before
changing it. Detailed implementation notes and the authoritative current tool
statistics remain in [`DEV.md`](../DEV.md); public naming and contribution rules
remain in [`CONTRIBUTING.md`](../CONTRIBUTING.md) and
[`NAMING_CONVENTIONS.md`](NAMING_CONVENTIONS.md).

Konnect is a Rust MCP server with KiCad integration and packaging support. An
MCP client talks to the `konnect` binary, which exposes a small starter surface,
loads domain toolsets on demand, and routes each call to a file, IPC, or
`kicad-cli` backend.

## Main Components

| Component | Source | Responsibility |
|---|---|---|
| Server binary | `crates/konnect` | CLI classification, configuration, installer/status commands, MCP transports, and plugin FFI exports |
| Core tool server | `crates/konnect-core` | MCP handling, routing, observability, tool definitions, and domain handlers |
| S-expression engine | `crates/konnect-sexp` | KiCad file parsing/writing, atomic edits, reversible commands, and transaction journals |
| Typed schematic editor | `crates/konnect-schematic-editor` | Higher-level schematic parse, mutation, library resolution, and serialization |
| KiCad IPC client | `crates/konnect-ipc` | Typed KiCad IPC messages, NNG transport, board targeting, and failure classification |
| Schematic viewer | `crates/schematic-viewer` | Separate Tauri application for live schematic rendering |
| KiCad launcher plugin | `plugin` | Python ActionPlugin and settings UI for the packaged server |
| Packaging | `packaging` | KiCad PCM metadata, package assembly, and package validation |

## Runtime Shape

The main ownership flow is:

```text
MCP client
  -> transport in crates/konnect/src/transport
  -> McpHandler in crates/konnect-core/src/mcp/handler.rs
  -> ToolRouter in crates/konnect-core/src/router
  -> handler in crates/konnect-core/src/tools
  -> one of:
       schematic model or S-expression file edit
       KiCad IPC request
       guarded closed-board file edit
       kicad-cli check or export
       external data/integration operation
```

The backend is selected per operation, not merely by file type. The write-path
decision and its safety gates are described in
[`KICAD_INTEGRATION.md`](KICAD_INTEGRATION.md) and implemented principally in
`tools/pcb_board.rs`, `tools/pcb_components.rs`, `tools/pcb_sync.rs`, and
`konnect-ipc/src/client.rs`.

## Reading Order

1. [Architecture](ARCHITECTURE.md) covers static ownership and the dynamic
   startup, routing, mutation, and recovery flows.
2. [Tool system](TOOL_SYSTEM.md) covers definitions, toolsets, dispatch,
   argument contracts, response evidence, and structured errors.
3. [Developing tools](DEVELOPING_TOOLS.md) is the implementation checklist for
   adding or changing a tool safely.
4. [KiCad integration](KICAD_INTEGRATION.md) explains the file, IPC, and CLI
   paths and the gates between them.
5. [Testing and release](TESTING_AND_RELEASE.md) maps changes to tests, CI, live
   KiCad validation, documentation, and packaging.

## Fork identity

This repository is `takurot/Konnect-xai`, a fork of the archived
`mixelpixx/Konnect`. Repository links in code come from one place,
`konnect_ipc::project_links`. `REPO_URL` is this fork and `UPSTREAM_REPO_URL`
is upstream. Runtime error text builds doc links with `docs_url`, and a test in
that module fails if any Rust source links to upstream docs again. Reliability
tracking issues may cite either repository.

**Decision: keep the PCM identifier `com.github.mixelpixx.konnect`.** The fork
is a drop-in replacement for upstream. KiCad's Plugin and Content Manager keys
a package by its identifier, and the identifier also sets the install directory
`com_github_mixelpixx_konnect`. The client configs that `konnect init` already
wrote record the binary's path inside that directory, as do the
`examples/*.json` configs and the README install paths. `runtime_info.rs`
matches the identifier to recognize a PCM install. Keeping it means a user can
replace an upstream install with this fork's package, and their AI client
configuration keeps working.

**Alternative not taken: rename the identifier**, for example to
`com.github.takurot.konnect-xai`. The fork would then be a separate PCM package
that can be installed beside upstream. The rename has to be applied together in
`packaging/metadata.json`, `plugin/plugin.json`,
`crates/konnect-core/src/runtime_info.rs`,
`crates/schematic-viewer/tauri.conf.json`, `examples/*.json`, `README.md`, and
`DEV.md`. Existing users would have to reinstall and edit their client configs.
Revisit this only if the two packages need to coexist.

The PCM metadata points `resources.homepage` and `author.contact.web` at this
fork. These are display links in the PCM schema and are independent of the
identifier. `author.name` stays `mixelpixx`, who wrote the plugin. The
`download_url` printed by `packaging/build-pcm.sh` and `build-pcm.ps1` points at
this fork's releases.

## npm distribution

`npm/` holds the launcher package `@konnect-xai/konnect` and one package per platform
(`konnect-linux-x64`, `-linux-arm64`, `-darwin-x64`, `-darwin-arm64`, `-win32-x64`). The
launcher resolves the installed platform package and runs its binary with inherited stdio.
Version and binaries are not committed: `.github/workflows/release.yml` (`publish-npm`) runs
`npm/scripts/version.js` (the tag must equal the Cargo workspace version), `stage.js` (puts
the release binaries into the platform packages and pins `optionalDependencies`), `smoke.js`
(pack, install, `--version`, MCP `initialize`), then `npm publish --provenance`.

Publishing uses npm Trusted Publishing, so no npm token is stored. Each of the six packages is
registered for the repository and workflow file with
`npm trust github <package> --repo takurot/Konnect-xai --file release.yml --allow-publish`.
Renaming `release.yml` breaks publishing until the registration is redone. A manual dispatch of
the workflow runs everything except the real publish (`--dry-run`). Test locally with
`node --test "npm/test/*.test.js"`.
