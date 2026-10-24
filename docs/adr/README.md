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
