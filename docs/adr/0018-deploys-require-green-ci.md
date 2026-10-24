# ADR-0018: Every deploy requires green CI on the exact commit

**Status:** accepted, 2026-10-24, after an external engineering review.
Amends ADR-0015 (the deploy workflow's gate).

## Context
`deploy.yml` deploys in two ways:
- **Automatically**, after a CI run of a push to `main` succeeds
  (`workflow_run`). This already required green CI, through the triggering
  run's own conclusion.
- **By hand** (`workflow_dispatch`, on `main` only). This checked only that
  the commit was still `main`'s tip. It deployed that commit whether its CI
  had passed, failed, was still running or had never run.

## Decision
- **One gate for both paths:** `scripts/ci-gate.mjs` runs before any tool
  install or deploy step. It asks the GitHub API for the CI workflow's runs
  on the commit and deploys only if all three hold:
  - the workflow ref is `refs/heads/main`;
  - the commit is `main`'s current tip;
  - the latest CI run for that commit, triggered by the push to `main`
    (not a pull request run, which tested another ref), has completed with
    conclusion `success`. The latest run is the highest run number, then
    the highest attempt.
- **What happens otherwise:**
  - A commit that is no longer the tip is skipped, with a notice and no
    failure: the newer commit's own run deploys it.
  - Everything else fails the job: failed, cancelled, running or missing
    CI, another ref, a malformed SHA. So does an unreachable or failing API
    (fail closed).
- **Permissions:** the workflow reads `actions` as well as `contents`; it
  writes nothing. The Cloudflare secrets still reach only the step that
  checks they exist and the deploy step.
- **The gate runs on the runner image's Node**, before mise installs the
  pinned tools: it uses only Node's core modules and the preinstalled `gh`,
  and running it first keeps the installers off the path of a refused
  deploy. This is a deliberate exception to "tools only from mise".
- **Migration order is unchanged:** migrations, then the Worker (ADR-0015).
  `docs/operations.md`, "Deploying safely", states the compatibility rule
  this order relies on, and what recovery looks like when a step fails.

## Consequences
- **No standard path deploys a commit CI has not passed.** A manual deploy of
  a commit whose CI is still running fails; run it again once CI is green.
- **The gate's own logic is tested with the GitHub API mocked**
  (`scripts/ci-gate.test.mjs`, run by CI). It covers:
  - green CI on the commit;
  - failed, cancelled and running CI;
  - a missing run;
  - green CI on another commit, ref or event;
  - a failed re-run after an earlier success;
  - a superseded tip;
  - another ref;
  - an API failure;
  - the command-line exit codes and outputs.
- **What it does not cover:** someone with write access who runs `wrangler
  deploy` by hand with the token. Keeping the token in the `production`
  environment, limited to `main` (README), is what bounds that.
