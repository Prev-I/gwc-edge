# ADR-0013: Hubs and caches are verified by the cron, not inline

**Status:** accepted, 2026-10-24. Records a choice that ADR-0003, ADR-0011
and ADR-0012 already rely on.

## Context
A hub's update could be checked while the request waits: open a TCP
connection to the claimed address, handshake, and store the hub only if it
answers. A submitted cache could be pinged the same way.

On the Workers Free plan that costs, on every update:
- **CPU:** 10 ms per invocation. The handshake's I/O does not count, but
  parsing and the second round of D1 work do.
- **Connections:** 6 at a time, shared with D1, and up to 8 s of latency in
  the reply Shareaza is waiting for. WinINet's own timeout makes a slow reply
  a failure, and two failures drop the cache.
- **Abuse:** an attacker could aim every update at a victim address and make
  the cache open a connection to it per request, bounded only by the rate
  limiter.

## Decision
Verification runs only in the 10-minute cron (ADR-0012):
- **The request path does no network I/O** of its own beyond D1 and the
  Rate Limiting binding. It stores what it is given (`unverified`,
  `pending`) and answers at once.
- **Everything is bounded per run, not per request:** the cron checks at most
  16 hubs and pings at most 2 caches, from queues it orders itself.
- **Request volume cannot raise the dialling rate.** Who gets dialled, and
  how often, is the cron's decision: backoff, reservations, and one check per
  `X-Try-Hubs` candidate.

## Consequences
- **Replies stay fast and never depend on a third party.**
- **New hubs wait.** A new hub is listed 10–20 minutes after its first
  update, not at once (ADR-0011). Shareaza registers a hub only after 2 h of
  stable uptime, so the delay is small by comparison.
- **The cache's outbound connections are bounded** whatever the request
  volume: at most about 2,300 handshakes and 288 pings a day.
