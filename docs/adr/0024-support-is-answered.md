# ADR-0024: `support=1` is answered with `i|support|gnutella2`

**Status:** accepted, 2026-11-13. Amends ADR-0008 (which parameters make a
protocol request), and changes the `support` row of `docs/protocol.md`
("Shareaza wiki: specification 2.1") from not supported to supported.

## Context
`support=1` is Skulls' extension for asking a cache which networks it serves.
Skulls 0.3.6 (SVN trunk r942,
<https://svn.code.sf.net/p/skulls/svn/trunk/skulls/>) answers it with one
`I|support|<network>` line per network it serves, after the pong and whatever
network the query names (`skulls.php:501-513`, `:1993-1994`). With
`support=2`, or `getnetworks=1`, it sends one `I|networks|…` line instead.
gwc-edge ignored the parameter, and a query carrying only `support` was not a
GWC request.

Sharelin, a G2 leaf for Linux and BSD, sends one request to a cache:
`?net=gnutella2&support=1&get=1&client=SHLN&version=<version>`. It uses the
reply only if it contains `i|support|gnutella2`, compared case-insensitively.
Otherwise it bans the cache, and its `discovery.dat` can keep the ban across
restarts. This is the same in 0.2.0 (SVN tag `0.2.0`, `src/discovery.cpp:113`,
`:264-324`) and in trunk r1160 (`src/g2net/discovery.cpp:120`, `:161-222`),
both at <https://svn.code.sf.net/p/sharelin/code/>. `SHLN 0.2.0` is in
production traffic: an estimated 88 hostfile gets (counters sampled 1 in 8)
between 14:00 UTC on 2026-11-09 and 04:59 UTC on 2026-11-10
(`docs/field-test-plan.md`, stage 4; `docs/operations.md`, "Client codes seen in
the field"). By its code, every Sharelin that got a reply from this cache
banned it; how many installs that was is not known.

Shareaza does not send `support=1`. The line that would add it is commented out
(`DS:1417`), and its parser has no case for `i|support|`: the line falls through
the `i|` branch unread (`DS:1631-1749`).

## Decision
- **A truthy `support` adds `i|support|gnutella2` to a get or ping reply.**
  - It comes after the pong, if there is one, and before any host or cache line.
  - It is sent on any network, as Skulls does: the line says what this cache
    serves, not what the query asked for.
  - Only the `support=1` form is implemented. `support=2` gets the same single
    line, not Skulls' `I|networks|…` form, which stays unsupported with
    `getnetworks`.
- **`support` alone is a GWC request** (`route::GWC_PARAMS`). It is answered
  with that one line. Like a lone ping, it counts as `Kind::Ping`, reads no D1
  row and is not rate-limited.
- **A rate-limited get keeps the line**, since it costs no D1 read.
- **Unchanged:**
  - An update is still answered as an update only, so `support` beside `ip=` or
    `url=` adds nothing. Skulls adds the line to updates too; this cache does
    not, and no client seen sends `support` with an update.
  - Denylisted clients still get an empty body.
- **Not chosen:**
  - **Answering only when `net=gnutella2`.** Skulls answers on any network, and
    the line describes the cache, not the request.
  - **Implementing the `I|networks|…` form too.** No client seen in the field
    sends `support=2` or `getnetworks=1`, and Shareaza only checks those lines
    when they are present (`DS:1706-1749`).

## Consequences
- **Sharelin keeps this cache after its first get**, and uses its hosts and
  caches.
  - A ban is saved to `discovery.dat` as a `B` line, and only at shutdown
    (trunk `src/g2net/discovery.cpp:51-55`, `:275-292`). A saved `B` line
    survives restarts while the file still holds an unbanned cache (`:62-85`),
    and adding the cache again by hand does not lift it
    (`AddCache`, `:265-269`): the line has to be edited or removed.
  - In memory, Sharelin drops every ban when it runs out of unbanned caches it
    may query: it clears its list and reloads its built-in defaults (`:94-103`,
    `:113-117`, `:225-232`). It can then learn this cache again from another
    cache's `u|` lines, so a banning install may well come back on its own.
  - Sharelin never submits, so it adds no hubs or caches here.
- **Shareaza is unaffected.** It never sends the parameter, and would ignore the
  line (`response::tests::shareaza_ignores_the_support_line`).
- **A query carrying only `support` now gets the protocol reply**, not the
  landing page or the 301.
- **`/stats` counts a lone `support` query as a ping**, and Sharelin's gets as
  hostfile gets, as before.
