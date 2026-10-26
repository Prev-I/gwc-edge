# ADR-0023: The cache ping carries a urlfile get

**Status:** accepted, 2026-10-26. Amends ADR-0012 ("Caches", the request).

ADR-0021 and ADR-0022 were proposed before this record, in a separate pull
request.

## Context
The cron pinged other caches with `?ping=1&net=gnutella2&client=TEST&version=…`
(ADR-0012). A ping counts only with an `i|pong|…` line (`ping::pong_line`).

Cachechu 1.6 (<https://github.com/kjotoole/cachechu>, commit `c0934a8`)
picks its reply format from the request:
- it treats a request with neither `get` nor `update` as GWC1
  (`index.php:126-130`);
- so it answered that ping with `PONG Cachechu 1.6` (`index.php:206-213`);
- so every Cachechu install was marked `failed`, then deleted after three
  pings.

Shareaza has no trouble with Cachechu: its G2 queries always pair `ping=1`
with `get=1` (`DS:1413-1434`, "Requests Shareaza sends" in
`docs/protocol.md`). Skulls 0.3.6 (SVN trunk r942,
<https://svn.code.sf.net/p/skulls/svn/trunk/skulls/>) works around the same
behaviour: it pings a Cachechu install again with `update=1` added
(`skulls.php:977-980`).

## Decision
- **The ping sends
  `?ping=1&get=1&urlfile=1&net=gnutella2&client=TEST&version=gwc-edge+<version>`.**
  - That is the shape of Shareaza's own G2 urlfile query, so a cache that
    answers Shareaza answers the cron in the same format.
  - The identification is unchanged: `client=TEST` and the cache's name.
- **A urlfile get, not a hostfile get.**
  - It matches the query Shareaza sends after a host query.
  - On a cache that honours `urlfile=1`, such as another gwc-edge, it is the
    cheaper request.
  - Cachechu and Skulls ignore `urlfile`, and answer any get with hosts and
    caches alike (`index.php:416`; `skulls.php:2134-2138`).
- **What counts as a pong is unchanged.** It is an `i|pong|…` line whose
  networks, when present, include `gnutella2`. A GWC1 `PONG` line is still
  not one: it says nothing about the networks a cache serves.
- **Not chosen:**
  - **Accepting `PONG <name>`.** It would verify caches that never said they
    serve G2.
  - **Retrying with `update=1`, as Skulls does.** It costs a second request,
    and sends an update that carries nothing to store. Worse, Cachechu runs
    its per-address update ban before it answers the ping. A second update
    from the same address within its ban period (55 to 60 minutes by
    default) gets `ERROR: Client returned too early` and no pong
    (`index.php:179-191`). Workers may share outbound addresses, so that could
    fail pings that have nothing to do with this cache.

## Consequences
- **Cachechu installs can be verified, and so listed**, if their first
  configured network is `gnutella2`, which is the default.
  - Cachechu joins its networks with `|`, not `-` (`index.php:208`).
  - An install configured `Gnutella, Gnutella2` therefore answers
    `I|pong|Cachechu 1.6|gnutella|gnutella2`.
  - That still fails here, as it fails in Shareaza (`DS:1648-1666`,
    `shareaza_model`).
- **Pinged caches now send hosts or URLs after their pong.**
  - The reply is judged as soon as a complete pong line arrives
    (`ping::Reader`). It is read up to 16 KiB at most (`dial::ping_socket`,
    `dial::ping_fetch`, `verify::PING_BODY_MAX`).
  - Cachechu and Skulls print the pong before any host or URL line, so the
    cron's cost does not change.
- **A pinged gwc-edge does more work.**
  - It counts the ping as a urlfile request (`counters::Kind::Urlfile`, not
    `Ping`), so its `/stats` shifts between kinds.
  - It consults its rate limit under the urlfile key, and reads its hostfile
    snapshot.
  - A rate-limited reply still carries the pong (`docs/protocol.md`,
    "Rate-limited gets"), so the verdict does not depend on the limit.
- **Caches deleted before this change** come back only if they are
  submitted again.
- **The integration tests' fake cache now answers like Cachechu.** It sends
  `PONG` unless the request carries `get` or `update`, so reverting the
  query fails `scripts/integration.sh`.
