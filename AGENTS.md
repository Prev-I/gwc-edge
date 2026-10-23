# rs-gwc-edge

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
migrations/         D1 migrations, applied in order by wrangler
scripts/smoke.sh    wrangler dev + local D1 + /healthz check (also run in CI)
wrangler.jsonc      Worker config; build runs worker-build
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
wrangler deploy --dry-run --outdir target/wrangler-dist   # build + bundle, no upload
wrangler d1 migrations apply DB --local
scripts/smoke.sh                     # SMOKE_PORT overrides the default 18787
```

CI (`.github/workflows/ci.yml`) runs all of the above on every pull request and
every push to `main`.

## Rules that are not obvious from the code

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
- **Stay inside one invocation's budget:** 10 ms CPU, 50 D1 queries, 50 external
  subrequests, 6 simultaneous connections (TCP sockets included).
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
