# ADR-0011: Hubs are listed only after a handshake

**Status:** accepted, 2026-10-24, after an expert review requested by the
reviewer. Supersedes ADR-0010's "listed at once" and "on the same port"
clauses, and amends ADR-0006's `unverified` cap.

## Context
A self-registered hub was listed at once, and a get serves the 20 most
recently seen hubs. A port change bypassed the 30-minute coalescing, so an
attacker holding about 20 IPv4 addresses could refresh each once a minute,
keep every served slot, and break the write bound. The source check proves
the IP; it does not prove a G2 hub listens on the port, so non-hubs (or a
victim's address) could be handed to every new leaf.

## Decision
- **The update path creates or demotes rows; only the cron promotes them.**
  A self-registration is stored `unverified`, source `update`, and served only
  after the cron's first successful handshake.
- **Coalescing is on time alone:** any row refreshed less than 30 minutes ago
  is not written, whatever its status or port.
- After 30 minutes: a servable hub on the same port is refreshed (fields and
  `last_seen`; status and failures untouched); on a new port it is demoted to
  `unverified`; an `unverified` or `dead` row is requeued as `unverified`.
- **Replies keep newest-first order with a diversity cap:** at most 1 hub per
  /24 and 4 per /16 in one reply.
- **The 150 `unverified` cap counts only `X-Try-Hubs` candidates**
  (source `try_hubs`); self-registrations are bounded by the 500-hub cap, so
  150 attacker addresses cannot shut the door on new hubs.
- For the verifying cron (next phase): re-handshake servable hubs every 1–2 h
  even if they keep refreshing; keep `dead` self-registered rows (deleting
  them would reopen an insert loop) and evict only old ones in bounded batches.
- Open for the verifying cron: whether a requeue resets `failures` (it keeps
  them today), and how to stop an attacker holding ~500 addresses from keeping
  the 500-hub cap full with requeued rows. Both are for that phase's design.

## Consequences
- A new hub appears 10–20 minutes after its first update; Shareaza registers a
  hub only after 2 h of stable uptime, so this is small.
- Until the verifying cron exists, no self-registered hub is served. There is
  no deployment yet; the verifying cron is a precondition of the first one.
- One write per hub per 30 minutes, for every status: the write bound holds.
- An attacker running real G2 hubs on many addresses still passes the
  handshake; the diversity cap bounds its share of a reply rather than
  eliminating it.
- The end-to-end "get shows the hub" test moves to the verifying cron's phase,
  with a fake hub in the test client to answer the handshake.
