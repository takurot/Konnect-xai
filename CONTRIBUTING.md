# Contributing to Konnect-xai

Use the [fork issue tracker](https://github.com/takurot/Konnect-xai/issues) for bug reports
and proposed changes. For a bug, include the Konnect commit or version, KiCad version,
actual and expected results, and a reproduction using a disposable project when possible.

Read [AGENTS.md](AGENTS.md), [governance](GOVERNANCE.md), and the
[branch and pull request workflow](docs/BRANCH_AND_PULL_REQUEST_WORKFLOW.md).
Start independent changes from this fork's current `main` and isolate each Issue
in a branch and worktree. State dependencies and keep each PR's unique diff reviewable.

## Development setup

Install the Rust toolchain pinned in `rust-toolchain.toml`, Protocol Buffers, and CMake.
See [DEV.md](DEV.md) and [Testing and release](docs/TESTING_AND_RELEASE.md)
for platform prerequisites and the quality gates.

## Pull request checklist

- Describe the changed behavior, design, compatibility, dependencies, and test results.
- Follow the [reliability contract](docs/RELIABILITY_CONTRACT.md) for tool behavior
  and [naming conventions](docs/NAMING_CONVENTIONS.md) for public names.
- For a regression, show that the test detects the original behavior and passes with
  the change. Distinguish real KiCad checks from fixtures and report skipped cases.
- If adding or removing tools, set `tool_count` in `router/registry.rs`, update the tool's
  row in [`tool-directory.md`](tool-directory.md), and run `cargo xtask fix-doc-counts`.
  The `doc_tool_counts` check verifies the resulting catalogue and documentation.

Contributions are under the fork's [AGPL-3.0-only license](LICENSE).
