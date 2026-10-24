# ADR-0015: Deployment renders its config; migrations are frozen

**Status:** accepted, 2026-10-24.

## Context
`wrangler.jsonc` is committed and shared by everyone who deploys the cache.
Their values differ: the D1 database id, the canonical URL, and the hostname
of the Custom Domain. If the committed file held one deployer's values,
another deployer who forgot to change them would deploy against them.

**Deploy-time settings.** Wrangler does take a different config file
(`--config`), but has no flag for a D1 database id. It can create D1
databases on deploy by itself ("automatic provisioning"). In that case it
writes the id back into the config file, which a CI checkout loses.

**Migration ordering.** `d1 migrations apply --remote` needs the database to
exist, and a new build may need its migration before it runs.

**Editing in place.** Migration 0001 was edited in place while no remote
database existed (AGENTS.md). Once a database has applied it, an edit would
fail only on fresh databases, and without warning.

## Decision
- **The committed `wrangler.jsonc` is a template:**
  - no `database_id`;
  - no route;
  - a placeholder `CANONICAL_URL`;
  - `workers_dev` and `preview_urls` off.
- **`scripts/deploy.sh` renders `wrangler.deploy.json`**, which is
  git-ignored, through `scripts/render-config.mjs`. It adds the database id,
  `CANONICAL_URL`, and one Custom Domain route for its host. It refuses a URL
  that is not plain HTTP on port 80.
- **Deploy order:**
  1. find the D1 database by name, or create it;
  2. render the config;
  3. apply the migrations;
  4. deploy;
  5. run `smoke.sh --prod`.

  The same script runs by hand and in `.github/workflows/deploy.yml`. The
  workflow runs it after CI passed on `main`, on the commit CI tested, and
  only while that commit is still `main`'s tip: CI runs finish out of order,
  and an older commit must not roll production back. Its secrets,
  `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`, live in a `production`
  environment limited to `main`, and only the two steps that need them see
  them; the variable `GWC_CANONICAL_URL` is the repository's. Without them
  the workflow only says so.
- **Until the cron's first run**, `/healthz` reports it as never run. The
  smoke check after a deploy accepts that, however many attempts the first
  deploy took; the daily `prod-smoke` check does not, so a cron that never
  runs is still caught.
- **CI checks the deploy path without deploying:** it renders the config,
  bundles with `wrangler deploy --dry-run` (`deploy.sh --dry-run`), and runs
  the renderer's tests.
- **Migrations are frozen from v0.1.0 on:**
  - `migrations/SHA256SUMS` lists every migration's hash. CI checks the
    hashes, and that every `*.sql` file is listed.
  - A schema change is a new `000N_*.sql` file, with its hash added.
  - An existing migration is never edited again.

## Consequences
- **Nothing deployment-specific is committed**, and two deployers cannot step
  on each other's values.
- **A forked repository deploys nothing** until its owner sets the secrets
  and the variable.
- **Every schema change must be backward compatible for one deploy.** The
  migration lands before the code, so the previous build briefly runs
  against the new schema.
