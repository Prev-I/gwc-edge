# ADR-0014: Plain HTTP, on one Custom Domain

**Status:** accepted, 2026-10-24.

## Context
**Shareaza cannot use HTTPS here.** Shareaza 2.7 fetches caches with WinINet
and uses a `u|` URL only if it starts with `http://` (`docs/protocol.md`,
`DS:1573`). Its TLS stack is too old for today's certificates in any case.

**`*.workers.dev` will not do.** Those addresses are HTTPS-only, so the cache
needs its own hostname, reachable on port 80.

**Some Cloudflare zone features break Shareaza's requests.** Shareaza sends
no `Accept` and no `Accept-Encoding` header, which bot and integrity checks
treat as suspicious. Shareaza in turn treats a redirect to HTTPS, a challenge
page, a 403, a missing reason phrase or a compressed body as a failure, and
drops the cache after two failures:
- Always Use HTTPS;
- HSTS;
- Bot Fight Mode;
- Browser Integrity Check;
- Security Level "I'm Under Attack";
- the managed WAF rules.

**Not all of them can be turned off per hostname.** Configuration Rules can
change Browser Integrity Check, Security Level and Automatic HTTPS Rewrites
for one hostname. They cannot change Always Use HTTPS or HSTS. On the Free
plan, Bot Fight Mode cannot be skipped by any rule.

## Decision
- **The cache serves plain HTTP**, on one Custom Domain: the host of
  `CANONICAL_URL`.
  - `workers_dev` and `preview_urls` are off.
  - No other route serves the protocol under another name (ADR-0008).
- **Zone settings:**
  - The deployment guide asks for the three zone-wide settings to be off.
  - It asks for one Configuration Rule, scoped to the hostname, for the
    per-hostname ones.
  - It recommends a zone dedicated to the cache, so the zone-wide settings
    cost nothing elsewhere (README, "Zone settings").
- **Verification:**
  - `scripts/smoke.sh --prod` replays Shareaza's request shapes with
    Shareaza's headers. It checks a 200 with a reason phrase, `text/plain`,
    no `Content-Encoding`, and the cache's own `u|` line.
  - The deploy script runs it after every deploy, and the `prod-smoke`
    workflow runs it every day.
- **Every protocol reply carries `Cache-Control: no-store, no-transform`**,
  so neither Cloudflare nor a proxy compresses or caches it. The pages (`/`,
  `/stats`, `/robots.txt`) are not protocol replies and carry their own
  `public, max-age` (ADR-0008).

## Consequences
- **No transport security.** Nothing a cache serves is secret, and every
  listed hub has passed a handshake from the cache itself (ADR-0011). An
  attacker on the path can still forge replies, as they could against every
  GWebCache Shareaza knows.
- **A zone shared with HTTPS sites loses their protections.** Always Use
  HTTPS, HSTS and Bot Fight Mode off are a real cost there; a dedicated zone
  avoids it. In a shared zone, a Single Redirect rule can send the other
  hostnames to HTTPS and a response-header Transform Rule can give them HSTS;
  only Bot Fight Mode stays a zone-wide cost.
- **A dashboard change can break Shareaza silently.** Only the daily smoke
  test notices, by opening an issue.
