# gwc-edge

`gwc-edge` is a Gnutella2 Web Cache (GWebCache v2) written in Rust. It runs on
the Cloudflare Workers Free plan and stores its state in Cloudflare D1.

A GWebCache is a small HTTP service that bootstraps peer-to-peer clients:
- G2 hubs register themselves with it;
- clients ask it for recent hubs and for other caches.

The compatibility target is Shareaza 2.7.x, which runs on almost every G2 hub
today. The G2 network has about 35 hubs, and of the 14 caches in Shareaza's
default list, only one still answers.

What it does:
- **Answers Shareaza's four request shapes** byte for byte, as its parser
  expects (`docs/protocol.md`). No reply can make a client drop the cache.
- **Lists only hubs it has verified itself.** A cron handshakes every
  registered hub over TCP before listing it, and re-checks each listed hub
  about every 90 minutes.
- **Follows hubs' hints, and checks other caches.** It reads `X-Try-Hubs`
  from hubs that answer, and pings other caches before listing them
  (ADR-0012).
- **Stays within the Free plan**: 100,000 requests and 100,000 D1 row writes
  a day. Gets are served from a cron-rendered snapshot; counters are sampled;
  rate limits use the Workers binding (`docs/adr/`).
- **Keeps answering when D1 is down or over quota.** The replies are still
  correct, just without hubs (ADR-0007).
- **Shows its own state**: a landing page at `/`, `/stats` (JSON) and
  `/healthz`, plus one structured log line per request
  (`docs/operations.md`).

Documentation:

| Document | What it covers |
|---|---|
| `docs/protocol.md` | The protocol as Shareaza speaks it, with citations, and conformance to the published specifications (`docs/spec/`) |
| `docs/adr/` | Every design decision, one record each |
| `docs/operations.md` | Logs, `/stats`, `/healthz`, expected failure modes |
| `docs/field-test-plan.md` | Seeding, a real Shareaza as leaf and hub, announcing the cache, a week of observation |

## Deploying

This takes about 15 minutes on a free Cloudflare account. You need:
- **A domain on Cloudflare** (a zone in your account). Shareaza 2.7 speaks
  only plain HTTP, and `*.workers.dev` is HTTPS-only, so the cache must have
  its own hostname.
  - **Prefer a zone of its own.** Two settings Shareaza needs off can only be
    changed for a whole zone (see "Zone settings").
- **This repository and the tools a deploy needs**, at their pinned versions:

  ```bash
  git clone https://github.com/Prev-I/gwc-edge && cd gwc-edge
  mise trust && mise install rust@1.99.1 node npm:wrangler cargo:worker-build
  ```

  A plain `mise install` also installs the development tools, including the
  fuzzing nightly, which takes longer.
- **bash 4 or later** for the scripts (macOS ships 3.2: install a newer one).

### 1. Choose the canonical URL

Pick one URL, for example `http://gwc.example.org/`. It must be:
- **plain `http://`**;
- **on port 80;**
- **a hostname in your zone**, with no existing DNS record. Cloudflare creates
  the record.

This is the URL every client will store, and the cache lists it in its own
replies.
- **Keep it stable.** A client that stored another URL for this cache will
  not recognise it as the same one.
- **Use only this one.** No `*.workers.dev` address, and no second hostname
  used as an alias (ADR-0008). Other hostnames routed to the Worker answer `/`
  with a redirect to this URL.

### 2. Deploy

Log in with `wrangler login`, or set `CLOUDFLARE_API_TOKEN` and
`CLOUDFLARE_ACCOUNT_ID` (see the token permissions in step 4). If your login
has access to more than one account, set `CLOUDFLARE_ACCOUNT_ID` either way:
the script runs wrangler non-interactively. Then run:

```bash
GWC_CANONICAL_URL=http://gwc.example.org/ scripts/deploy.sh
```

The script does four things:
1. **The database:** creates the D1 database `gwc-edge`, on the first run
   only.
2. **The migrations:** applies them before the new code goes live.
3. **The Worker:** deploys it, with its 10-minute cron and a Custom Domain
   for the hostname.
4. **The check:** runs `scripts/smoke.sh --prod`, which sends Shareaza's
   request shapes with Shareaza's headers.

The committed `wrangler.jsonc` is never edited. The script renders
`wrangler.deploy.json` from it, adding the database id, the URL and the route
(ADR-0015).

### 3. Zone settings

Until the cron's first run, within 10 minutes of the first deploy, `/healthz`
answers 503 with `"cron":"never_run"`. The deploy script accepts that; the
daily check does not.

Several Cloudflare zone features reject or rewrite Shareaza's requests.
Shareaza sends no `Accept` and no `Accept-Encoding` header. It treats a
redirect to HTTPS, a challenge page, a 403, a missing reason phrase or a
compressed body as a failure, and drops the cache after two failures. If the
smoke check in step 2 failed, this is why.

| Setting | Needed | Where |
|---|---|---|
| Always Use HTTPS | **Off** | SSL/TLS → Edge Certificates. Zone-wide: Configuration Rules cannot turn it off for one hostname. |
| HTTP Strict Transport Security (HSTS) | **Off**, or at least not covering this hostname (`includeSubDomains`) | SSL/TLS → Edge Certificates. Zone-wide. |
| Bot Fight Mode | **Off** | Security → Bots. Zone-wide: on the Free plan it cannot be skipped per hostname. |
| Browser Integrity Check | Off for this hostname | Rules → Configuration Rules, scoped to `http.host eq "gwc.example.org"` |
| Security Level | Essentially Off for this hostname (never "I'm Under Attack") | The same Configuration Rule |
| Automatic HTTPS Rewrites | Off for this hostname | The same Configuration Rule |
| Managed WAF rules | Must not challenge or block this hostname | Security → WAF. Add an exception if `smoke.sh --prod` shows a 403 or a challenge. |

One Configuration Rule, matched on the hostname, covers the three
per-hostname settings. The zone-wide ones are the reason to give the cache a
zone of its own. If the zone must stay shared with HTTPS sites:
- a Single Redirect rule (`http.host ne "gwc.example.org" and not ssl`, to
  HTTPS) restores Always Use HTTPS for the other hostnames;
- a response-header Transform Rule restores their HSTS;
- Bot Fight Mode stays off for the whole zone.

ADR-0014 explains why the cache is exposed over plain HTTP at all.

Check the result at any time:

```bash
scripts/smoke.sh --prod http://gwc.example.org/
```

### 4. Deploy from GitHub (optional)

`.github/workflows/deploy.yml` deploys `main` after CI passes on it, or when
started by hand on `main`. Either way it first checks, through the GitHub
API, that the commit is `main`'s tip and that CI passed on that exact commit
(ADR-0018):
- a commit that is no longer the tip is skipped, so a slow CI run cannot
  roll production back;
- a commit whose CI failed, is still running or never ran is refused.

In the repository settings:
1. **Create an environment `production`** (Settings → Environments), with
   Deployment branches limited to `main`.
2. **Add the two secrets to that environment**, not to the repository, so no
   other branch can read them:

   | Secret | Value |
   |---|---|
   | `CLOUDFLARE_API_TOKEN` | An API token with Account: Workers Scripts Edit, D1 Edit; Zone (this zone): Workers Routes Edit, DNS Edit, SSL and Certificates Edit, Zone Read. A Custom Domain creates a DNS record and an edge certificate. |
   | `CLOUDFLARE_ACCOUNT_ID` | Your account id |

3. **Add a repository variable** (Settings → Secrets and variables →
   Variables): `GWC_CANONICAL_URL`, the canonical URL from step 1. The
   `prod-smoke` workflow reads it too.

`.github/workflows/prod-smoke.yml` runs `smoke.sh --prod` every day. When it
fails it opens an issue labelled `prod-smoke` (Issues must be enabled), or
comments on the one already open; a later success closes it. Dashboard
changes to zone settings break Shareaza without failing anything else, which
is why this check exists. GitHub disables scheduled workflows after 60 days
without repository activity: re-enable it in the Actions tab when that
happens.

### 5. After deploying

These signs say the cache is healthy (`docs/operations.md`):
- `/healthz` reports `"status":"ok"`, `"cron":"ok"` and `"test_dial":false`;
- `/stats` shows the cron's last run and the request counts;
- the Workers Logs show one JSON line per request, plus one line per cron
  run.

## Getting listed

A new cache has no hubs and no clients until others know about it.
- **Other caches.** Submit this cache's URL to working caches with
  `?update=1&url=<canonical URL>&net=gnutella2&client=…`. Once their own
  verification has pinged this cache, they list it. `dkac`
  (`http://dkac.trillinux.org/dkac/dkac.php`) is the one known to be alive.
  [`docs/field-test-plan.md`](docs/field-test-plan.md) covers this step, and
  the stages before it:
  - seeding this cache from `dkac` with `scripts/seed.sh`;
  - testing it with a real Shareaza, as a leaf and as a hub;
  - a week of observation.
- **Shareaza's default list.** Open a pull request against
  [`ansani/Shareaza`](https://github.com/ansani/Shareaza) adding one line to
  `Data/DefaultServices.dat`, in its Gnutella2 block:

  ```
  2 http://gwc.example.org/
  ```

  The leading `2` marks a G2-only cache.
  - **Not `M`, which marks a cache as G1 and G2.** Shareaza would then also
    send it G1 gets, which this cache answers with nothing but its own URL.
  - **Give the cache time first.** Let it run for a few weeks, with
    `prod-smoke` green, before asking.

## Development

Every tool is pinned in `mise.toml`, including one dated nightly that is used
only for fuzzing:

```bash
mise install
cargo test --workspace
wrangler d1 migrations apply DB --local
wrangler dev                         # http://127.0.0.1:8787/healthz
scripts/smoke.sh                     # build, start, check /healthz and the --prod checks
scripts/integration.sh               # seeded end-to-end tests, a Shareaza-shaped client and fake peers
scripts/fuzz.sh 60                   # fuzz each parser for 60 s (cargo fuzz, nightly)
```

`AGENTS.md` lists the rules that are not obvious from the code:
- the D1 budget;
- one log line per request;
- migrations are frozen from v0.1.0 on.

## Licence

MIT. See `LICENSE`.

The exception is the copied files in `docs/spec/`: verbatim copies of the
published GWebCache specifications, Copyright (c) 2003 Hauke Dämpfling and
distributed under the GNU Free Documentation License 1.1, together with that
licence's text. See `docs/spec/README.md`.
