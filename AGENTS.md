# gwc-edge

Instructions for all AI coding agents (Claude Code, Codex, OpenCode).

## Project Overview

`gwc-edge`: a Gnutella2 Web Cache (GWebCache v2) in Rust on Cloudflare Workers
(Free plan), storing state in Cloudflare D1. Shareaza 2.7.x is the
compatibility oracle: where the written GWC spec and Shareaza's parser
disagree, Shareaza wins, and the deviation is recorded in `docs/protocol.md`
(written in phase 2) with a citation into Shareaza's `DiscoveryServices.cpp`.

```
crates/gwc-core/    pure protocol logic, no `worker` dependency, >= 90% line coverage
crates/gwc-worker/  thin Workers adapter: routing, D1, cron (cdylib, wasm32)
crates/gwc-testclient/  native client that speaks exactly as Shareaza 2.7.10.6;
                    its tests/ run against wrangler dev (scripts/integration.sh);
                    src/bin/fake_peer.rs answers the cron's handshakes and pings
migrations/         D1 migrations, applied in order by wrangler
docs/protocol.md    the protocol authority: GWC v2 as Shareaza speaks it, with citations
docs/adr/           one record per decision; change a decision with a new record
docs/spec/          verbatim copies of the published GWC specs (GNU FDL), never edited; README.md is ours
docs/operations.md  logs, /stats, /healthz, and the expected failure modes
docs/field-test-plan.md  how to prove the deployed cache with real Shareaza clients and hubs
scripts/smoke.sh    wrangler dev + local D1 + /healthz and --prod checks (also run in CI);
                    `--prod <url>` checks a deployed cache as Shareaza sees it
scripts/deploy.sh   render wrangler.deploy.json, migrate, deploy, smoke --prod (ADR-0015)
scripts/ci-gate.mjs the deploy workflow's gate: main's tip with green CI on that commit (ADR-0018)
scripts/fuzz.sh     cargo fuzz on the dated nightly; fuzz/ is its own workspace
scripts/seed.sh     one-shot seeding from another cache (dkac) via the gwc_seed tool
scripts/integration.sh  wrangler dev + isolated seeded D1 + cron run + gwc-testclient tests
wrangler.jsonc      Worker config; build runs worker-build
wrangler.no-d1.jsonc  tests only: the Worker without D1, for degraded-mode tests
mise.toml           every tool, pinned; CI installs from the same file
mise.lock           checksums and URLs for the tools mise downloads; `mise lock` refreshes it
.mise/locks/        wrangler's npm dependency lock, referenced from mise.lock by digest
```

## Commands

Tools come only from `mise.toml` (`mise install`). Never install Rust tooling
another way, and pick stable releases. After changing `mise.toml`, run
`mise lock` and commit both `mise.lock` and `.mise/locks/`, deleting version
directories under `.mise/locks/` that `mise.lock` no longer references.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p gwc-worker --target wasm32-unknown-unknown -- -D warnings
cargo deny check
cargo test --workspace
cargo llvm-cov -p gwc-core --summary-only --fail-under-lines 90
GWC_CANONICAL_URL=http://gwc.example.org/ scripts/deploy.sh --dry-run   # render + bundle, no upload
node --test 'scripts/*.test.mjs'
scripts/fuzz.sh --replay             # every committed fuzz input once (CI); scripts/fuzz.sh 60 to fuzz
wrangler d1 migrations apply DB --local
scripts/smoke.sh                     # SMOKE_PORT overrides the default 18787
scripts/integration.sh               # GWC_PORT overrides the default 18789
```

CI (`.github/workflows/ci.yml`) runs all of the above on every pull request and
every push to `main`.

## Rules that are not obvious from the code

- **`docs/protocol.md` is the protocol authority.** If a brief or a request
  conflicts with it, stop and ask; never change a reply Shareaza parses without
  checking it against `shareaza_model` and the cited source.
- **Every D1 call made while answering a request goes through `Probe::run`**,
  with a statement name (the cron and the post-response writes are the
  exceptions, and log their own failures). The first failure switches the
  request to degraded mode: no further D1 access, and a reply that is still
  protocol-correct (ADR-0007). A write the reply does not depend on (the
  snapshot write-back) uses `Probe::try_run`: logged and counted, not degrading.
- **Exactly one `console_log!` line per request** (`gwc_core::log::RequestLine`),
  on every path, and never the requester's IP. D1 errors add one error-level
  line by statement name (ADR-0009).

- **Logic goes in `gwc-core`.** Anything testable without the Workers runtime
  (parsing, formatting, validation, handshake parsing, time math) belongs there,
  written test-first. `gwc-worker` only moves data between HTTP, D1 and core.
- **Never `strip` the release profile.** It removes the custom sections
  wasm-bindgen needs and the build fails with "externref table required for
  catch wrappers".
- **Count D1 row writes.** The Free plan allows 100,000 rows written per day and
  fails every query beyond it until 00:00 UTC. Each row a statement changes is
  one written row, an in-place `UPDATE` included, and each index on a changed
  column adds one more. Aggregating counters by hour bounds storage, not writes.
- **The request path writes nothing** unless a read in the same invocation
  showed a row will change. The exceptions are the sampled request counter
  (1 request in 8 writes `n = n + 8`) and the health flush (at most two rows
  per isolate per minute, only when there are D1 errors or degraded replies to
  record); both run after the response and never from a degraded request.
- **Gets read one row**, the cron-rendered hostfile snapshot, or none while the
  isolate remembers it (60 s). Only when that snapshot is missing or stale
  (cron stalled) does a get scan `hubs` and `caches`. It then writes the
  snapshot back, guarded on the stale value it read, so concurrent isolates
  write once and the next requests read it again: about one scan per 30 min
  overall while the cron is down. `/stats` serves only the cron-rendered
  document (503 before the first run), grouped in SQL by the cron.
  `crates/gwc-worker/src/store.rs` holds every statement and its cost; the
  header of migration 0001 states the rules.
- **Stay inside one invocation's budget:** 10 ms CPU, 50 D1 queries, 50 external
  subrequests, 6 simultaneous connections (TCP sockets included). The cron
  counts every socket, fetch and D1 statement against a ledger
  (`gwc_core::verify::Ledger`, ceiling 45); a new statement in the cron's
  path must be added to `cron::TAIL` or `store::*_STATEMENTS` (ADR-0012).
- **Only the cron changes a hub's status to listed or `dead`, writes
  `last_checked`, or resets `failures`**; no request deletes a row
  (ADR-0012).
- **`TEST_DIAL` is for `scripts/integration.sh` only.** Never put it in a
  `wrangler*.jsonc`; a core test enforces it.
- **Migrations are frozen** (ADR-0015): `migrations/SHA256SUMS` lists each
  file's hash and CI checks it. A schema change is a new
  `migrations/000N_*.sql`, plus its line in `SHA256SUMS`; never edit an
  existing one. Core tests check that the CHECK constraints match core's
  bounds (`Kind`, `MIN_PORT`); keep it that way when adding columns.
- **The committed `wrangler.jsonc` is a template** (ADR-0015): never commit a
  `database_id`, a route or a real `CANONICAL_URL`. `scripts/deploy.sh`
  renders `wrangler.deploy.json` (git-ignored) with them.
- **Pin exact versions** (`=x.y.z`) in `[workspace.dependencies]`, and pin
  GitHub Actions by commit SHA.

## Conventions

- Code, commit messages, branch names and documentation in **English**.
- Conventional commits: `feat:`, `fix:`, `docs:`, `chore:`, `refactor:`,
  `test:`. Keep commits atomic.
- Integration follows `.repository-policy.yaml`: work goes on a branch and
  reaches `main` through a pull request.
- `AGENTS.md` is the only place agent guidance is written; `CLAUDE.md` stays a
  one-line `@AGENTS.md` import.
- No secrets, tokens, API keys or connection strings in any committed file.
