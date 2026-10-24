# Architecture decision records

One file per decision, numbered in the order they were taken. A decision is
changed by a new record that supersedes the old one, never by editing it.

| ADR | Decision |
|---|---|
| [0001](0001-d1-for-all-state.md) | D1, not KV, holds all state |
| [0002](0002-shareaza-is-the-oracle.md) | Shareaza is the protocol oracle; no reply may cost the cache its place |
| [0003](0003-hostfile-from-a-cron-rendered-snapshot.md) | Gets are served from a cron-rendered snapshot |
| [0004](0004-sampled-request-counters.md) | Request counters are sampled, 1 in 8 |
| [0005](0005-rate-limiting-binding.md) | Rate limiting uses the Workers Rate Limiting binding |
| [0006](0006-capacity-caps-and-who-may-enter.md) | Capacity caps, and what may enter `hubs` |
| [0007](0007-degraded-mode.md) | Degraded mode: the protocol minimum, no further D1 access |
| [0008](0008-landing-page-and-non-protocol-routes.md) | Landing page, robots, methods, alias hosts |
| [0009](0009-one-log-line-per-request.md) | One structured log line per request |
| [0010](0010-the-write-path.md) | The write path: what an update stores (partly superseded by 0011) |
| [0011](0011-hubs-are-listed-after-a-handshake.md) | Hubs are listed only after a handshake; coalescing on time; diversity cap (amended by 0012) |
| [0012](0012-the-verifying-cron.md) | The verifying cron: queue, budget, `dead` rows, `X-Try-Hubs`, cache pings, test seam |
| [0013](0013-verification-runs-in-the-cron.md) | Hubs and caches are verified by the cron, not inline |
| [0014](0014-plain-http-on-a-custom-domain.md) | Plain HTTP, on one Custom Domain; the zone settings Shareaza needs |
| [0015](0015-deployment-and-frozen-migrations.md) | Deployment renders its config; migrations are frozen from v0.1.0 |
