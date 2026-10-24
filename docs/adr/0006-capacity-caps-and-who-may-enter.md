# ADR-0006: Capacity caps, and what may enter `hubs`

**Status:** accepted, 2026-10-23; enforced by the write path (ADR-0010); the
unverified cap and deletion on first failure come with the verifying cron.

## Context
The G2 network has about 35 hubs. Fake registrations cost writes on every
refresh, and IPv6 gives an attacker effectively unlimited source addresses.

## Decision
- At most 500 hubs, of which at most 150 `unverified`, and 50 caches (3 per
  domain). Caps are checked against the `counts` table (one row read per cap),
  incremented in the same batch as each insert and reconciled by the cron.
- An `update=1&ip=…` is stored only when `ip=` equals the connecting address
  (`CF-Connecting-IP`). Otherwise it is answered `OK` and never stored, not even
  as `unverified`.
- `unverified` rows from `X-Try-Hubs` in the verifier's handshakes count
  against the 150 cap, are not served until verified, and are deleted on their
  first failed check. Since ADR-0011, self-registered hubs are also stored
  `unverified` until verified, but count only against the 500-hub cap.

## Consequences
- Worst-case hub writes stay around 27,000 a day.
- A hub behind NAT that reports a private address is not listed (it could not
  be reached anyway).
