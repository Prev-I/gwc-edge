# Field test plan

How to prove that a deployed `gwc-edge` works for real Shareaza clients and
hubs, and how to bring it into the G2 network. Stage 1 was first run against
production on 2026-10-25; the table at the end records what each stage found.

Each stage has:
- what to do;
- what to watch;
- what counts as passing.

Record the results in the table at the end. Stop at the first stage that
fails, and use `docs/operations.md` to find out why.

## Before you start

- [ ] **The cache is deployed** (README, "Deploying"), and
  `scripts/smoke.sh --prod <canonical URL>` passes.
- [ ] **`/healthz`** shows `"status":"ok"`, `"cron":"ok"` and
  `"test_dial":false`.
  - After the very first deploy, the cron trigger can take a while to start:
    on 2026-10-25 the first run came 17 minutes after the deploy.
  - Until then `/healthz` reports `"cron":"never_run"` (`"pending"` if
    nothing has been requested yet) and `/stats` answers 503. Wait for the
    first run before stage 1.
- [ ] **The `prod-smoke` workflow** is green, if you deploy from GitHub.
- [ ] **Workers Logs are open:** Workers & Pages → gwc-edge → Observability.
  Also open the `/stats` page.
- [ ] **A Windows machine or VM with Shareaza 2.7.10.x.**
  - Prefer the official 2.7.10.2, which most of the G2 network runs and
    which sends `client=RAZA`.
  - The `docs/ORACLE.md` of github.com/Prev-I/Shareaza covers running
    2.7.10.2 and that fork's 2.7.10.6 under Wine on Linux. WSL2 is untested
    there.
  - The 2.7.10.6 build sends `client=RAZI`, not `RAZA`. The same document
    warns that it has known security flaws. Stage 3 keeps the client running
    as a public hub for hours: stay with it, or use 2.7.10.2.
  - For stage 3 you also need:
    - a public IPv4 address, not behind carrier-grade NAT;
    - one TCP port forwarded to it (Shareaza's default is 6346).
- [ ] **A backup of Shareaza's `Discovery.dat`** (in Shareaza's data folder),
  so you can restore the client's own cache list afterwards.

Throughout, `http://gwc.example.org/` stands for your canonical URL.

## Stage 1: seed the cache

A new cache knows no hubs. Seed it once from `dkac`, the one other G2 cache
known to be alive, then let the cron verify what came in:

```bash
GWC_CANONICAL_URL=http://gwc.example.org/ scripts/seed.sh fetch seed.sql
less seed.sql                       # review: one INSERT per hub and cache
scripts/seed.sh apply seed.sql      # stores exactly that file
```

`apply` writes to the deployed D1, so wrangler must be authenticated on the
machine that runs it:
- either `wrangler login`;
- or `CLOUDFLARE_API_TOKEN` (with D1 Edit) and `CLOUDFLARE_ACCOUNT_ID` in the
  environment.

The deploy workflow's token lives only in GitHub and cannot be used here.
A `seed.sql` at the repository root is git-ignored.

The script:
- **Asks `dkac` once each** for its G2 hostfile and urlfile, in `fetch`.
  `fetch` fails and writes nothing if `dkac` offers no hub. An error page or
  a refusal therefore cannot pass for a seed.
- **Keeps what this cache's own rules accept:**
  - public IPv4 hubs on port 1024 or above, at most 100;
  - canonical `http://` caches, at most 3 per host, never this cache; `dkac`
    itself is one of them;
  - no cache the cron deleted after failed pings in the 48 hours before
    `fetch` ran (ADR-0022). The bound is fixed when `fetch` writes the file,
    so a file applied later errs towards skipping.
- **Stores them for verification only.** Hubs go in as `unverified` (source
  `seed`) and caches as `pending`. Nothing is served before the cron has
  checked it.
  - Seeds share the cron's queue with new registrations. A full seed of 100
    hubs takes about an hour of runs, during which a hub registering itself
    may wait longer than usual.
  - `apply` stores the file with a D1 query, not an import, so the running
    cache is not interrupted. If `apply` reports an error, nothing was
    stored: fix the cause and run it again on the same file.

A read-only dry run on 2026-10-24 found 11 hubs and 20 caches. The
production fetch on 2026-10-25 got the same counts and wrote 11 hub and 19
cache inserts.

**Watch**, over the next three cron runs (30 minutes):
- the `cron` log line: `picked` near 11 at first, then `verified`,
  `alive_full` and `dead`;
- one `cron_hub` line per hub, with `class: "new"`;
- one `cron_cache` line per cache pinged, 2 a run;
- a `cron_leaks` line after any run that completes with leaked sockets;
- on `/stats`, `hubs.served`.

`/stats` also carries the last run's `cron` figures, the same fields as the
`cron` log line, so the counters can be followed without the logs.

**Passing:**
- at least one seeded hub is `verified` or `alive_full`;
- a G2 hostfile get lists it:

  ```bash
  curl -s -A 'Shareaza 2.7.10.6' -H 'Accept:' -H 'Accept-Encoding:' \
    'http://gwc.example.org/?get=1&hostfile=1&net=gnutella2&ping=1&client=GWCEsmoke'
  ```

  `GWCEsmoke` is the client code `smoke.sh --prod` sends, so these requests
  are not counted as a real client's.

- the `cron` line shows `verification_failed: false` and `subrequests` at
  most 45;
- **leaked sockets only where expected** (ADR-0019):
  - `leaked_sockets` is at most the run's `cron_hub` lines with `result:
    "timeout"`, plus its `caches_failed`;
  - `skipped_budget` is above 0 only when `leaked_sockets` reached 5 or
    `subrequests` reached 45.

## Stage 2: Shareaza as a leaf

This checks that a client which knows only this cache can bootstrap from it.

The menu paths below are those of Shareaza 2.7's windowed mode. In the
tabbed mode most of these windows are under **View → Advanced** instead.
Menu names may differ slightly between builds.

1. **Close Shareaza.** Back up `Discovery.dat` if you have not yet.
2. **Start Shareaza with G2 only:** turn off Gnutella, eDonkey and the
   others in the network settings.
3. **Make this cache the only one.** Open the Discovery window (F9, or
   View → Discovery). Remove the other GWebCaches, and the UDP host caches
   too (UKHL entries), which also hand out hubs.
4. **Add this cache.** Right-click in the Discovery window and choose
   **Add Service…**. Set **Type** to **G2 GWebCache**, then enter the
   canonical URL exactly.
   - **The dialog starts on *G1 GWebCache*.** Left there, the cache is
     flagged G1 only (`docs/protocol.md`, "Failures and removal"). With
     G1 off since step 2, Shareaza would then never ask it.
   - *G1/G2 GWebCache* also works: with G1 off, only G2 queries are sent.
     A `gwc:` link adds the cache that way (`docs/protocol.md`, "Failures
     and removal").
5. **Empty the host cache** (View → Host Cache, then clear it), so that
   Shareaza has to ask a cache for hubs.
6. **Check the list again.** Just before connecting, make sure the Discovery
   window still lists only this cache. Shareaza may restore its default
   services when its list gets short.
7. **Connect.**

**Watch:**
- **Workers Logs:** within a minute, a `hostfile` line, then a `urlfile`
  line right after it. Shareaza sends the urlfile query only while it knows
  fewer than 10 caches. Both lines should have `client: "RAZA"` (`"RAZI"`
  for the 2.7.10.6 fork), the Shareaza version, `outcome: "ok"`, and for
  the hostfile `hosts_sent` above 0.
- **Shareaza's Discovery window:** this cache's access count goes up, and its
  failure count stays at 0. Two failures in a row remove a cache from
  Shareaza's list, entirely.
- **Shareaza's System window** (View → System): it reports the cache's reply
  and then connects to the hubs it listed.

**Passing:**
- **A hub from this cache.** Shareaza connects to at least one G2 hub, and
  that hub's address is among those in this cache's hostfile reply. Run the
  `curl` from stage 1 and compare.
- **No failure** for this cache in the Discovery window after two queries.
  Shareaza waits at least 30 minutes between queries to the same cache, so
  check again after half an hour.

**If it fails:**
- the access count rises, but so does the failure count;
- no log line appears at all.

Run `scripts/smoke.sh --prod`. It names a redirect, a challenge, compression
or a missing reason phrase. Then check the zone settings (README, ADR-0014).

## Stage 3: Shareaza as a hub

This checks the write path and the verifying cron with a real hub: the
project's definition of done, outside the test suite.

1. **Make Shareaza a hub.** In the G2 settings, set it to hub mode, with the
   forwarded port as its listening port.
2. **Keep this cache among its working caches.** Shareaza sends updates only
   to caches it has used successfully, with at least one access and no
   failures (`docs/protocol.md`, "When Shareaza talks to a cache").
   - Stage 2's urlfile reply also taught Shareaza the caches this one lists,
     and any of them that works can receive the update instead.
   - The first update may therefore go elsewhere for a few hours. Removing
     the other caches from the Discovery window again shortens that.
3. **Leave it running.** Shareaza registers a hub only once it has been a
   stable hub for 2 hours with at least 4 neighbours, and then at most once
   an hour (`docs/protocol.md`, "When Shareaza talks to a cache").

**Watch:**
- **The update:** an `update` log line with `ip:inserted`. A
  `url:inserted`/`url:known` part appears too if Shareaza submitted a cache.
  If you see `ip:source_mismatch`, the address Shareaza reports is not the
  one it connects from (NAT, or IPv6), so the cache stores nothing.
- **The check, within 10 to 20 minutes:** a `cron_hub` line with `class: "new"`,
  `result: "200"` (or `"503"` when the hub is full) and `now: "verified"`
  (or `"alive_full"`).
- **The listing:** a hostfile get now lists the hub. On `/stats`,
  `hubs.verified` (or `alive_full`) went up by one.

**Passing:**
- the hub is listed within 20 minutes of its first update, or within about
  an hour if a full seed was applied just before;
- it stays listed while it keeps running. It is re-checked every 90
  minutes, and its updates refresh it hourly.

**If it fails:**
- a `cron_hub` line with `result: "timeout"` or `"unreachable"` means the
  port is not reachable from the internet: check the forwarding;
- `503` with `Shielded leaf node` or `Need an Ultrapeer` means Shareaza was
  not in hub mode;
- `GWC_TRACE=1` (a Worker variable, set briefly) logs each reply's headers.

## Stage 4: announce the cache

Ask `dkac` to list this cache:

```bash
curl -s -G -A 'gwc-edge' 'http://dkac.trillinux.org/dkac/dkac.php' \
  --data 'update=1&net=gnutella2&client=TEST' \
  --data-urlencode 'version=gwc-edge (announce)' \
  --data-urlencode 'url=http://gwc.example.org/'
```

**Do this once.** Caches limit how often one address may update them. If
the reply is not an `OK`, wait an hour before trying again.

**Watch:**
- **dkac's verification ping:** soon afterwards, a `ping` request line in
  this cache's logs whose `client` is neither `RAZA` nor `RAZI`. The logs
  never carry the requester's address, so the client code is how dkac's
  request shows.
- **dkac's listing:** later, this cache in dkac's urlfile:

  ```bash
  curl -s 'http://dkac.trillinux.org/dkac/dkac.php?get=1&urlfile=1&net=gnutella2&client=TEST&version=gwc-edge'
  ```

**Passing:** dkac lists the canonical URL. From then on, Shareaza clients
that use dkac learn this cache too, and `hostfile` and `urlfile` requests
from other addresses start to appear in the logs.

## Stage 5: a week in the network

Leave the cache running for a week. Then look at the following:

| What | Where | Healthy |
|---|---|---|
| Uptime as Shareaza sees it | `prod-smoke` workflow, `/healthz` | green every day; `status: ok`, `cron: ok` |
| Hubs listed | `/stats` `hubs.served` | close to the G2 network's hub count (about 35, http://crawler.doxu.org/) |
| `dead` rows | `/stats` `hubs.dead` | stable or slowly varying; not climbing towards 1,000 |
| Requests | `/stats` `requests` | mostly `RAZA` or `RAZI` hostfile and urlfile gets (`client` `GWCE`, `version` `smoke`, is `smoke.sh --prod` or the stage 1 `curl`); `limited` and `denied` small |
| D1 health | `/stats` `health` | `d1_errors_24h` and `degraded_responses_24h` at 0 |
| D1 budget | Cloudflare dashboard → D1 → gwc-edge → Metrics | rows written well under 100,000 a day (expected about 5,000); rows read under 5 million |
| Cron cost | `cron`, `cron_hub` and `cron_leaks` log lines | `ms` under 120,000 (it includes `slot_wait_ms`, the waits for leaked sockets' slots, ADR-0020; the final wait for leaked sockets comes after it, in `cron_leaks` `waited_ms`); `subrequests` under 45, which also settles whether sockets count as subrequests; `leaked_sockets` only as stage 1 allows (ADR-0019); `skipped_budget` above 0 only with `subrequests` at 45 or `slot_wait_ms` of at least 15,000 (ADR-0020 tightens stage 1's rule: a leaked socket's slot comes back once its close succeeds) |
| CPU | Workers & Pages → gwc-edge → Metrics | CPU time per invocation well under 10 ms, cron included |
| Logs | Workers Logs | no `cron_failed`, `internal_error` or unexpected `d1_error` lines |

**Passing:**
- every row above is healthy for the whole week;
- the `prod-smoke` workflow opened no issue.

Then propose the cache for Shareaza's default list (README, "Getting
listed"): one line, `2 http://gwc.example.org/`, in `Data/DefaultServices.dat`
of `ansani/Shareaza`.

## Afterwards

**Restore Shareaza's own cache list** (the backed-up `Discovery.dat`) on the
test machine, so that it does not keep relying on one cache.

**Do not take a cache down casually.** Clients that know it drop it after
two failed requests, and they do not add it back on their own. To pause it,
keep it answering: a deploy that serves no hubs still keeps its place.

## Results

| Stage | Date | Result | Notes (counts, anomalies, links to logs) |
|---|---|---|---|
| 1. Seed | 2026-10-25 | Passed (under ADR-0019's criterion) | 11 hubs and 19 caches seeded from dkac (counted in the seed file; the caches match `/stats`). Cron runs at 20:50, 21:00 and 21:10 UTC: `picked` 11, 16, 15; `verified` 5, 0, 7; `alive_full` 4, 1, 5; `dead` 2, 5, 3; `deleted` 0, 5, 3; `candidates_added` 19, 2, 15; `subrequests` 37, 42, 41; `verification_failed` false throughout. After the first run a G2 hostfile get listed 9 hubs; after the third `hubs.served` was 22. Caches: 4 verified, 2 failed, 13 pending. **Initially not passing** (the first criterion): `leaked_sockets` was 1, then 5, then 0. With 5 leaked, the second run left 10 hubs for later (`skipped_budget` 10). Every other criterion held. Cause found (ADR-0019): a socket whose connection never opened cannot be closed early. The 21:30 and 21:40 runs, the first logged per hub, meet the new criterion: 5 leaked sockets for 5 timeouts, with 2 hubs skipped once all 5 slots had leaked; then 2 for 2, none skipped. The earlier runs were not logged per hub. **Confirmed** over the runs at 22:20, 22:30 and 22:40 UTC, with `cron_leaks`: `leaked_sockets` 1, 3, 0 for 1, 3, 0 timeouts (plus 2, 1, 0 failed pings); `skipped_budget` 0 throughout; `subrequests` 35, 33, 24; `hubs.served` 24, 23, 23; every leaked socket closed during the wait, its connection open 19,280 to 19,507 ms from the connect call (ADR-0020). |
| 2. Leaf | | | |
| 3. Hub | | | |
| 4. Announce | | | |
| 5. One week | | | |
