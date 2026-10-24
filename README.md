# rs-gwc-edge

`gwc-edge` is a Gnutella2 Web Cache (GWebCache v2) written in Rust and run on
the Cloudflare Workers Free plan, with Cloudflare D1 for storage.

A GWebCache is a small HTTP service that bootstraps peer-to-peer clients: G2
hubs register themselves, and clients ask it for recent hubs and for other
caches. Compatibility target is Shareaza 2.7.x, which runs on almost every G2
hub today.

**Status:** under construction. Reads and writes work: `ping`, hostfile and
urlfile gets served from a cron-rendered snapshot, hub registration and cache
submission with per-source rate limits and caps, a landing page at `/`,
`/stats`, `/healthz` and `/robots.txt`, and a degraded mode that keeps
answering correctly when D1 fails. Handshake verification, the deployment
guide and the field-test plan arrive in later phases.

- `docs/protocol.md`: the protocol, as Shareaza speaks it.
- `docs/adr/`: the decisions behind the design.
- `docs/operations.md`: logs, counters and what to do when things go wrong.

## Development

Every tool is pinned in `mise.toml`:

```bash
mise install                         # Rust, wrangler, worker-build, cargo-deny, ...
cargo test --workspace
wrangler d1 migrations apply DB --local
wrangler dev                         # http://127.0.0.1:8787/healthz
scripts/smoke.sh                     # build, start, and check /healthz
scripts/integration.sh               # seeded end-to-end tests, Shareaza-shaped client
```

## Licence

MIT. See `LICENSE`.
