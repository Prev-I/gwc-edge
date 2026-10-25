# Operating gwc-edge

How to see what the cache is doing, and what to do when it goes wrong.
`docs/adr/` explains why things are the way they are; this page is about
running it.

## Where to look

| Question | Look at |
|---|---|
| Is it up, and is the cron running? | `GET /healthz` |
| How many hubs and caches, how many requests, any D1 trouble? | `GET /stats`, or the landing page `GET /` |
| What happened to one request, or in the last hour? | Workers Logs |
| Does Shareaza still get through the zone's protections? | `scripts/smoke.sh --prod <canonical URL>`; the daily `prod-smoke` workflow, which opens an issue labelled `prod-smoke` on failure |
| What was deployed, and did it pass its checks? | The `deploy` workflow run: migrations, deploy, then `smoke.sh --prod` |

`/stats` and `request_log` are the durable record. Workers Logs is the
debugging channel: 7 days, at most 200,000 events a day on the Free plan.

## Logs

Follow live:

```bash
wrangler tail --format pretty            # everything
wrangler tail --format json | jq 'select(.logs[].message[0] | fromjson? | .outcome == "degraded")'
```

In the dashboard: Workers & Pages → gwc-edge → Observability → Events. Filter on
`outcome`, `kind` or `event`.

**One line per request**, on every path:

| Field | Meaning |
|---|---|
| `kind` | GWC kinds as in `request_log` (`ping`, `hostfile`, `urlfile`, `update`, `other_net`, `limited`, `denied`, `error`), or a page kind (`page`, `stats`, `healthz`, `robots`, `redirect`, `method_not_allowed`, `not_found`, `config_error`, `internal_error`). |
| `client`, `version` | From the query; `-` when absent. |
| `scheme` | `http` or `https`. |
| `outcome` | `ok`, `rate_limited` (over a per-source limit: kind `limited`), `denied`, `degraded`, `malformed`. |
| `rate_limit` | Only `unavailable`, and only on a request that should have been rate limited but was let through because the Rate Limiting binding was missing or failed (fail-open, ADR-0005). Absent otherwise. |
| `hosts_sent`, `caches_sent` | `h|` lines, and `u|` lines other than the cache's own. |
| `d1_ms` | Time spent waiting on D1. |
| `cf_colo` | Cloudflare data centre. |
| `snapshot` | For gets: where the hostfile came from (`memo`, `stored`, `live`, `stale`, `fallback`). |
| `update` | For updates: what each part did, as labels, e.g. `ip:inserted url:domain_full`. `ip:` is `inserted` (stored `unverified`), `refreshed`, `demoted` (new port: back to `unverified`), `requeued` (an `unverified` hub queued again for the cron), `reregistered` (a `dead` hub queued for a new check after its backoff; still `dead`), `backoff` (a `dead` hub within its backoff: nothing written), `queued` (a `dead` hub already waiting for a check), `coalesced`, `full`, `error`, or a decline (`source_mismatch`, `not_public`, `bad_port`, `malformed`, `not_g2`); `url:` is `inserted`, `known`, `full`, `domain_full`, `error`, or a decline (`https`, `invalid`, `self`, `not_g2`). No address or URL. |

No line carries the requester's address.

**Other lines** are not requests: they carry an `event`, no `outcome`.
- `{"level":"error","event":"d1_error","statement":…,"error":…}`: a D1 call failed. `statement` is a name (`read_snapshot`, `live_snapshot`, `binding`, …), never SQL; `error` is D1's own text.
- `{"level":"error","event":"internal_error",…}`: a handler failed; the request line follows with kind `internal_error`.
- `{"level":"error","event":"cron_failed",…}`: the cron failed.
- `{"level":"warn","event":"rows_dropped",…}`: rows that could not be decoded were skipped.
- `{"level":"warn","event":"snapshot_unparsable",…}`: a stored document this build cannot read; the cron re-renders the stats part on its next run.
- `{"level":"warn","event":"count_rows_written",…}`: a counter write billed other than 1 row (the budget assumes 1).
- `{"level":"warn","event":"rate_limiter_unavailable",…}`: the Rate Limiting binding failed; requests are allowed (fail-open). Logged once per isolate, naming the binding, so it does not say how long the failure lasted: count the request lines with `rate_limit: "unavailable"` for that.
- `{"kind":"cron",…}`: one per cron run (ADR-0012). Fields:
  `picked` (hubs picked for a handshake), `verified`, `alive_full`, `dead` (verdicts),
  `deleted` (failed `X-Try-Hubs` candidates), `candidates_added`, `purged`
  and `evicted` (`dead` rows removed), `caches_pinged`, `caches_verified`,
  `caches_failed`, `skipped_budget` (picked but not dialled), `leaked_sockets`
  (sockets whose close timed out, usually because their connection never
  opened; each shrinks the run's concurrency, ADR-0019),
  `verification_failed` (the verification's D1 work failed: see the
  `d1_error` line; nothing it decided was stored), `subrequests`
  (counted against the run's ceiling of 45), `test_dial`, `ms`,
  `hubs_served`, `caches`, `stats_rendered`.
- `{"kind":"cron_hub",…}`: one per hub checked: `ip`, `port`, `class` (`new`,
  `recheck`, `returning`, `candidate`), `was` (status before), `result` (the
  reply's status code, or `unreachable`, `timeout`, `closed`, `invalid`,
  `blocked`), `now` (status after, or `deleted`), `try_hubs` (entries
  offered). `ip` is set only for a listing (a hub that was or is now listed,
  an `X-Try-Hubs` candidate, a seed); for a self-registration that never
  passed a check it is `null`, since that address is only a requester's.
- `{"kind":"cron_cache",…}`: one per cache pinged: `url`, `result`
  (`verified`, `failed`, `deleted`).
- `{"kind":"cron_leaks",…}`: after the `cron` line of a run that completes
  with leaked sockets (ADR-0019): `leaked`, `settled`, `open_ms` (for each
  close that completed, how long its connection stayed open, from the
  connect call, sorted; never below about 9,000, the handshake deadline
  plus the close timeout), `unsettled` (still open when the wait ended),
  `waited_ms` (about 30,000 at most).
- `{"kind":"cron_trace",…}`: only with `GWC_TRACE=1`: each hub's full reply
  status and headers. Off by default. Not written for a hub whose `ip` is
  `null`: its headers can carry its address (`Listen-IP`).

A panic writes no request line; it appears as an uncaught exception in the
invocation log.

## `/stats`

| Field | Meaning |
|---|---|
| `hubs.served` | Hubs a hostfile lists now: `verified` plus `alive_full`. |
| `hubs.verified`, `alive_full` | Servable hubs seen within the 2 h listing window, at the last cron run. |
| `hubs.unverified`, `dead` | All hubs with that status, whatever their age. |
| `hubs.total` | Every row in `hubs`, expired ones included. |
| `caches.*` | Rows in `caches` by status. |
| `cron` | The last cron run: when, how long, and its counts (the fields of the `cron` log line, ADR-0012). `test_dial: true` means an integration-test configuration. |
| `requests` | The last 24 h, estimated from 1-in-8 sampling (`sample_rate`). |
| `health.d1_errors_24h` | D1 calls that failed. |
| `health.degraded_responses_24h` | Replies that were not the full reply: a D1 failure, a copy remembered after one, a configuration or internal error. |
| `generated_at` | When the hourly part (`requests`, `health`) was rendered. |

## `/healthz`

`status` is `ok` (200), `degraded` (503) or, with a missing or invalid
`CANONICAL_URL` or `TEST_DIAL`, 500. `test_dial` must be `false` in
production: when `true`, the cron dials only loopback test peers, so every
real check fails (ADR-0012). `cron` is:

| Value | Meaning |
|---|---|
| `pending` | No cron run yet and no traffic: a fresh deploy. |
| `never_run` | Requests were served but no cron run has completed: check the trigger. |
| `stalled` | The last completed cron run is over 30 minutes old (three missed runs). |
| `ok` | Running. |

## Expected failure modes

**Zone protection blocks Shareaza.** Shareaza sends no `Accept` and no
`Accept-Encoding` header. Browser Integrity Check, Bot Fight Mode and managed WAF
rules may answer it with a 403 or a challenge; to Shareaza that is a failure, and
two failures delete the cache from the client.
- Symptom: `/stats` requests fall while the cache looks healthy in a browser;
  few `hostfile` lines in the logs.
- Check: `scripts/smoke.sh --prod <canonical URL>`, which replays Shareaza's
  four request shapes with its headers and names what is wrong (status line,
  content type, compression, missing self line). The `prod-smoke` issue holds
  the same output.
- Fix: the zone settings in the README deployment guide (ADR-0014). A 301 to
  `https://` is Always Use HTTPS; a 403 or an HTML challenge is Bot Fight
  Mode, Browser Integrity Check, Security Level or a WAF rule.

**D1 quota exhausted.** D1 fails every query until 00:00 UTC.
- Symptom: `outcome: degraded` lines and `d1_error` lines; `/healthz` `database: error`;
  `health` counts rising in `/stats` once D1 is back.
- Effect: clients still get a successful reply (pong and the cache's own URL),
  just no hubs. Nothing to do but wait for 00:00 UTC; then find what used the
  budget (`d1_ms`, request volume by `kind`).

**Cron over its CPU budget or failing.** The Free plan gives the cron 10 ms of CPU.
- Symptom: `/healthz` `cron: stalled` or `never_run`; `cron_failed` lines;
  gets logging `snapshot: live` (the read path rebuilt the snapshot itself).
- Effect: hostfiles still work, from the live fallback, at more D1 reads.
- Fix: look at the cron's `ms` in its log line and at what it scans.

**Cron over its subrequest budget.** Every socket, fetch and D1 statement
counts against a ceiling of 45 of the 50 the Free plan allows.
- Symptom: `skipped_budget` above 0 in the `cron` line; new hubs listed later
  than 10–20 minutes after their first update. `skipped_budget` also counts
  hubs and pings left out because sockets leaked (`leaked_sockets` above 0):
  closes that timed out may hold connection slots. Hubs skipped that way
  still count in `subrequests`, which is charged before dialling.
  - A leak follows a handshake or ping whose connection never opened. The
    runtime cannot close such a socket until the attempt fails (ADR-0019).
  - Leaks up to the run's `timeout` results plus its `caches_failed` are
    expected; more is a bug.
  - The `cron_leaks` line says how long they stayed open.
- Effect: the hubs left over wait for the next run (10 minutes).
- Fix: compare `subrequests` with the 50 limit; the queue's reserved shares
  (`gwc_core::verify::Class::reserved`) and `HANDSHAKES_PER_RUN` set the load.

**Hubs flapping between listed and `dead`.** A listed hub becomes `dead` after
two failed handshakes in a row, 10 minutes apart.
- Check: its `cron_hub` lines (`result`); turn on `GWC_TRACE=1` briefly to
  see its replies' headers.
- A `503` with `Shielded leaf node`, `Need an Ultrapeer`, `Ultrapeer
  disabled` or `Wrong Protocol` means it is not a hub; `timeout` or
  `unreachable` usually means a firewall or a host that went away.

**Rate limiter unavailable, or limits exceeded.** The Rate Limiting binding
is approximate: Cloudflare counts per location ("local to the Cloudflare
location that your Worker runs in"), eventually consistent across
instances, and a client can exceed the limit before the counts converge.
Traffic spread over many locations, or many source addresses, is not held
to the configured rates. When the binding is missing or fails, every
request is let through (fail-open, ADR-0005): rejecting Shareaza's requests
would cost the cache its place in their lists (ADR-0002).
- Symptom: request lines with `rate_limit: "unavailable"`, one
  `rate_limiter_unavailable` warning per isolate; or, with the binding
  working, a rising request count from few clients with no `limited` lines.
- What it can cost, with no limiter at all:
  - **Gets** read the snapshot, 0 or 1 row each, and write nothing but the
    sampled counter. The 100,000 requests a day are the binding limit, well
    before the 5 million rows read.
  - **Updates** stay bounded by what the write path allows regardless of the
    limiter: one write per hub per 30 minutes (coalescing), new hubs only
    from their own address and only up to 500 live, caches up to 50 and 3 per
    host (ADR-0006, ADR-0010, ADR-0012). An attacker with many addresses can
    still spend writes faster than with the limiter.
- Check: Workers Logs, filtered on `rate_limit`; the D1 dashboard's rows
  written per day; `/stats` `requests.by_kind` and `limited`.
- Fix: a missing binding is a deployment fault (the rendered
  `wrangler.deploy.json` lists `ratelimits`); redeploy. For abuse from a few
  vendors, `CLIENT_DENYLIST`. A strict global limit would need a Durable
  Object, which this design deliberately avoids.

**Verification failing on D1.** The `cron` line says
`verification_failed: true`, after a `d1_error` line naming `cron_queue` or
`apply_cron`.
- Effect: nothing the checks decided is stored, and `last_checked` is not
  written, so the next runs dial the same hubs again (every 10 minutes) until
  it is fixed. Snapshots and `/stats` still update.
- Check: the `d1_error` text; the usual cause is the D1 quota (see above).

**`TEST_DIAL` in production.** `/healthz` shows `test_dial: true` and every
`cron_hub` line says `blocked` or names a loopback failure; nothing new is
ever listed. Remove the variable.

## Deploying safely

`scripts/deploy.sh` applies the migrations, then deploys the Worker
(ADR-0015). The deploy workflow runs it only for `main`'s tip with green CI
on that commit (ADR-0018). Order matters, because of what can fail between
the two steps.

**Schema changes are expand, then contract.** A migration lands while the
previous Worker is still serving, and stays if the new Worker never
deploys. So every migration must work with the Worker already running:
- **Expand** in one release: new tables, new nullable or defaulted columns,
  and new CHECK values the old code never writes. In SQLite a changed CHECK
  means rebuilding the table, which rewrites every row and its indexes:
  count those rows against the day's write budget, and run such a migration
  early in the UTC day.
- **Contract** in a later release, once no deployed code reads the old shape
  any more: dropping or renaming a column, tightening a CHECK.

Migrations are frozen once released (`migrations/SHA256SUMS`): a mistake is
fixed by a new migration, never by editing one.

**When the migrations succeed but the deploy fails** (permissions, Custom
Domain, build): production runs the previous Worker on the new schema, which
expand-only migrations keep safe. Fix the cause and run the deploy again.
`d1 migrations apply` skips the migrations already applied.

**When the deploy succeeds but the smoke check fails:**
- Read the output of `smoke.sh --prod`. A redirect, a challenge or
  compression is a zone setting (README, "Zone settings"), not the code;
  fix the zone, then run `scripts/smoke.sh --prod <url>` again.
- A failing reply from the Worker itself is the code: roll the Worker back
  (below), then investigate.

**Rolling back.** The Worker and the database roll back separately:
- **The Worker:** Workers & Pages → gwc-edge → Deployments, then roll back to
  an earlier version, or `wrangler rollback`. Cloudflare keeps the 100 most
  recent versions. "Resources connected to your Worker will not be changed
  during a rollback": D1 keeps its schema and data, so the older code must
  still work with them. Expand-only migrations guarantee that.
- **The schema:** never by reverting a migration file. Write a new migration
  that undoes the change, as a forward step.
- **The data:** D1 Time Travel restores the whole database to a minute in
  the last 7 days (Free plan; 30 days on Paid):
  - `wrangler d1 time-travel info gwc-edge` gives the current bookmark;
  - `wrangler d1 time-travel restore gwc-edge --bookmark=<bookmark>` (or
    `--timestamp=…`) restores it.

  A restore is destructive. It overwrites the database in place and cancels
  in-flight queries, which the Worker answers in degraded mode. It returns a
  bookmark of the state it replaced, so it can be undone.

**Before a destructive migration** (anything a contract step drops or
rewrites):
1. note the Time Travel bookmark;
2. export the database with
   `wrangler d1 export gwc-edge --remote --output=backup.sql`;
3. keep the export outside the repository.

The cache's data can be rebuilt (hubs re-register within hours, and
`scripts/seed.sh` refills an empty cache), so the export is for
convenience. Without the bookmark, though, a bad migration costs the hours
of rebuilding.

None of this runs automatically: restores, exports and rollbacks are
operator actions.

**Deploy refused by the gate** (ADR-0018). The step "main's tip, with
green CI?" fails with an `::error::` saying why:
- **CI still `in_progress` or `queued`:** wait for it, then run the deploy
  workflow again by hand on `main`.
- **CI failed or was cancelled, or there is no CI run on main** for the
  commit: fix CI. A commit pushed while Actions was disabled never got a
  run; push again or re-run CI.
- **GitHub API unavailable:** the gate fails closed; run it again later.
- **A ref other than `main`:** only `main` deploys.

A notice "no longer the tip of main" is not a failure: the newer commit's
own run deploys it.

**Deploy fails.** The `deploy` workflow stops at the first failing step of
`scripts/deploy.sh`.
- At `render-config`: `GWC_CANONICAL_URL` is not a lowercase `http://` URL
  on a DNS name, port 80, ending in a path.
- At `d1 create` or `migrations apply`: the API token lacks D1 Edit, or the
  account is over its database quota.
- At `deploy`: the token lacks Workers Scripts Edit or the zone permissions
  for the Custom Domain, or the hostname already has a DNS record (a Custom
  Domain needs the name free).
- At the smoke check, with the deploy itself done: zone settings, as above.
  The script retries for two minutes first, since a new Custom Domain takes
  a moment to answer.

**A migration changed after release.** CI fails at "migrations are frozen":
an existing file under `migrations/` no longer matches `SHA256SUMS`. Revert
it and add a new `000N_*.sql` instead (ADR-0015).
