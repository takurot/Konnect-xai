# Branch and Pull Request Workflow

Start independent work from the latest `main` of
[takurot/Konnect-xai](https://github.com/takurot/Konnect-xai). Use one branch
and worktree per Issue. Keep the base checkout and unrelated work intact.

## Dependent changes

Record the dependency and the unique scope of each branch. Base independent
PRs on the fork's `main`. A stacked PR can target the prerequisite branch
so its diff shows the dependent change. After the prerequisite lands, update the
dependent branch and base, then verify the resulting diff and checks again.
Use a disposable integration branch to test coupled changes when necessary.

## Review requests and completion

Follow the six stages in [AGENTS.md](../AGENTS.md). Include the Issue,
acceptance evidence, and any unverified cases in the PR. Review the diff at
the current head and resolve actionable findings. A code-owner request routes
review. Use the fork's actual GitHub protection and CI state when checking readiness.

The maintainer authorizes merge, release, and deployment. See [Governance](../GOVERNANCE.md)
for the fork's review and scope rules and [Testing and release](TESTING_AND_RELEASE.md)
for runtime and packaging validation.
