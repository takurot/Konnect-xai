# Konnect-xai governance

[@takurot](https://github.com/takurot) maintains this fork and owns decisions
about scope, releases, and repository settings. [CODEOWNERS](.github/CODEOWNERS)
routes review requests. Discuss work in the [fork issue tracker](https://github.com/takurot/Konnect-xai/issues).

## Contributions and review

Follow [AGENTS.md](AGENTS.md) and the [branch and pull request workflow](docs/BRANCH_AND_PULL_REQUEST_WORKFLOW.md).
Review the exact PR head, check required CI and unresolved findings, and obtain maintainer
authorization for merge. Release and deployment each require explicit authorization.

## Proportionate scope review

Resolve the demonstrated problem within its supported workflow. For an expansion, record the
evidence, smallest coherent fix, and behavior it would change. The maintainer resolves scope
questions that affect public contracts or compatibility.

## Evidence

Record checks for the affected behavior and distinguish simulated, hosted CLI, and live-editor
results. When adding a guard, verify that its regression check fails with the guard disabled.
A missing environment-specific check should be documented with its impact on acceptance.

## Licensing

This fork is [AGPL-3.0-only](LICENSE) and has no contributor license agreement or
commercial license. The [historical upstream guidance](docs/upstream/README.md) records
the original project's own governance.
