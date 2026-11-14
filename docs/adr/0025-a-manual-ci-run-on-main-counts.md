# ADR-0025: A CI run started by hand on main counts for the deploy gate

**Status:** accepted, 2026-11-14 (pull request #34). Amends ADR-0018 (which
CI runs count).

## Context
Someone who wants their own instance can fork this repository and deploy it
from the fork's GitHub Actions (README, "Run your own (from a fork)"). Under
ADR-0018 that fails at the first deploy:
- **The gate counts only a CI run of a push to `main`.** A fork starts with
  none: creating a fork pushes nothing, and workflows do not run in a fork
  until its owner enables them.
- **`ci.yml` could not be started by hand.** It ran only on pushes to `main`
  and on pull requests.
- So a manual deploy of a fresh fork is refused with "no CI run on main", and
  the only way out was to push a commit of the fork's own.
- **Updating has the same gap.** GitHub's documentation does not say whether
  "Sync fork" triggers `push` workflows. If it does not, the synced commit
  has no CI run the gate accepts.

## Decision
- **`ci.yml` can be started by hand** (`workflow_dispatch`).
- **The gate counts a CI run on `main` for the exact commit whose event is
  `push` or `workflow_dispatch`** (`COUNTED_EVENTS` in `scripts/ci-gate.mjs`).
  Both test the commit itself. A pull request run still does not count (it
  tested a merge ref), and neither does any other event.
- **The latest counted run decides**, across both events, by run number and
  then attempt, as before.
- **`deploy.yml` also starts after a green manual CI run** (`workflow_run`
  with event `workflow_dispatch`), still only for `main`
  (`branches: [main]`).
- **The runs are fetched without an event filter** and filtered by the gate,
  so manual runs are not missed.
- Everything else in ADR-0018 holds: only `main`'s tip deploys, a superseded
  tip is skipped, anything else fails closed, and the gate runs before any
  tool install.

## Consequences
- **ADR-0018's guarantee is unchanged:** no standard path deploys a commit
  that is not `main`'s tip, or one without green CI on that exact commit.
- **A fork deploys without a commit of its own:** enable Actions, run `ci` by
  hand on `main`, and the deploy follows. After a sync that started nothing,
  run `ci` by hand again.
- **Who can start CI by hand is who can push** (write access), so this opens
  no new way in.
- **A manual run acts like a re-run.** A later failed manual run blocks a
  deploy that an earlier green push run allowed, and a later green manual run
  allows one that a failed push run blocked.
- **A green manual run on another branch does nothing:** `deploy.yml` reacts
  only to runs on `main`, and the gate counts only those.
- **Tests** (`scripts/ci-gate.test.mjs`): green manual CI on the tip deploys;
  manual CI on another branch, and CI from `schedule`, `repository_dispatch`,
  `merge_group` or `pull_request`, does not count; the latest run decides
  across push and manual runs; the API is asked for every event.
