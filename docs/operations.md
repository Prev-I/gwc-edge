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
| `outcome` | `ok`, `rate_limited`, `denied`, `degraded`, `malformed`. |
| `hosts_sent`, `caches_sent` | `h|` lines, and `u|` lines other than the cache's own. |
| `d1_ms` | Time spent waiting on D1. |
| `cf_colo` | Cloudflare data centre. |
| `snapshot` | For gets: where the hostfile came from (`memo`, `stored`, `live`, `stale`, `fallback`). |

No line carries the requester's address.

**Other lines** are not requests: they carry an `event`, no `outcome`.
- `{"level":"error","event":"d1_error","statement":…,"error":…}`: a D1 call failed. `statement` is a name (`read_snapshot`, `live_snapshot`, `binding`, …), never SQL; `error` is D1's own text.
- `{"level":"error","event":"internal_error",…}`: a handler failed; the request line follows with kind `internal_error`.
- `{"level":"error","event":"cron_failed",…}`: the cron failed.
- `{"level":"warn","event":"rows_dropped",…}`: rows that could not be decoded were skipped.
- `{"level":"warn","event":"snapshot_unparsable",…}`: a stored document this build cannot read; the cron re-renders the stats part on its next run.
- `{"level":"warn","event":"count_rows_written",…}`: a counter write billed other than 1 row (the budget assumes 1).
- `{"kind":"cron",…}`: one per cron run, with its counts and duration (`ms`).

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
| `cron` | The last cron run: when, how long, and its verification counts. |
| `requests` | The last 24 h, estimated from 1-in-8 sampling (`sample_rate`). |
| `health.d1_errors_24h` | D1 calls that failed. |
| `health.degraded_responses_24h` | Replies that were not the full reply: a D1 failure, a copy remembered after one, a configuration or internal error. |
| `generated_at` | When the hourly part (`requests`, `health`) was rendered. |

## `/healthz`

`status` is `ok` (200), `degraded` (503) or, with a missing or invalid
`CANONICAL_URL`, 500. `cron` is:

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
- Check: replay a Shareaza request with `curl -A 'Shareaza 2.7.10.6' -H 'Accept:' -H 'Accept-Encoding:'`.
- Fix: the zone settings in the README deployment guide.

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
