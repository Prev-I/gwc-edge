# ADR-0017: Cache URL names that cannot be public; DNS rebinding remains

**Status:** accepted, 2026-10-24, after an external engineering review.
Tightens the cache-URL rules of `docs/protocol.md` ("Cache URLs").

## Context
Submitted cache URLs are stored `pending`, pinged by the cron and, once
verified, handed to every Shareaza client that asks for caches. The URL
rules already refuse non-public numeric hosts and the numeric forms WinINet
would read as a LAN address (`127.1`, `0x7f.0.0.1`). They accepted any
well-formed DNS name, including names that cannot be public by their form:
- `printer.local`, `db.internal`, `localhost.localdomain`;
- names that wildcard DNS services resolve to the address written in them,
  such as `10.0.0.1.nip.io`, `a-10-0-0-1-b.sslip.io` or
  `app-c0a80001.nip.io`.

### Threats, and what already stands against them
| Threat | Who is at risk | Already mitigated by |
|---|---|---|
| **SSRF from the Worker** (the cron pings a URL that points at an internal service) | Cloudflare's network, the cache's own account | The platform, for sockets: "Outbound TCP sockets to Cloudflare IP ranges are blocked", and "examples of a disallowed address include Cloudflare IPs, `localhost`, and private network IPs" (developers.cloudflare.com/workers/runtime-apis/tcp-sockets/). For `fetch()`, which pings DNS names on port 80, the documented restriction is only that it cannot address an IP directly (…/workers/platform/known-issues/); that it cannot reach a private network is assumed from Workers running on Cloudflare's network, not documented. The cron also sends one fixed `GET ?ping=1…` per check, reads at most 16 KiB and follows no redirect. |
| **A listed URL that points into the client's network** (the name resolves, or later re-resolves, to a private address) | Shareaza users: their client sends `GET <url>?get=1…` to a host on their LAN | Partly, by this ADR: names that are non-public by form are refused. Not at all for names that change their DNS answer (rebinding). |
| **Cache poisoning** (junk or hostile caches crowd out real ones) | The network's bootstrap | Verification before listing, removal after 3 failed pings, 50 caches and 3 per host (ADR-0006, ADR-0012). |
| **False positives** (a legitimate cache refused) | The cache's own coverage | Kept small: only names whose last label is special-use or never delegated, and only embedded addresses that are non-public. |

## Decision
- **Refuse, at admission, DNS names that cannot be public by their form**
  (`cache_url::normalize`, so submissions, seeding and `CANONICAL_URL`
  alike):
  - **Special-use and internal last labels:** `localhost`, `test`,
    `example`, `invalid` (RFC 6761), `local` (RFC 6762), `onion` (RFC 7686),
    `alt` (RFC 9476), `arpa` (reverse zones, `home.arpa`, RFC 8375),
    `internal` (reserved by ICANN), and the never-delegated LAN names
    `localdomain`, `lan`, `home`, `corp`.
  - **Embedded non-public addresses, read as wildcard DNS services read
    them:**
    - four decimal octets in a row, separated by `.` or `-` in any mix,
      anywhere in the name;
    - eight hex digits between separators.

  Only whole labels count for the last-label rule, and only whole tokens
  between separators for addresses: `local.example.org`,
  `gwc.home-server.net` and `deadbeef.example.com` stay valid. Embedded public
  addresses (`8.8.8.8.nip.io`) stay valid too. The rule and its test vectors
  (`crates/gwc-core/testdata/hostnames.json`) are shared with
  `scripts/render-config.mjs`, so a `CANONICAL_URL` the Worker would refuse
  fails before deploying.
- **No DNS lookup at admission or before listing.** Workers have no DNS
  API, so a lookup would need an external resolver (DNS over HTTPS), one
  more subrequest per check. It would also prove nothing about the answer a
  client gets later. A one-time lookup does not stop rebinding, so it is
  not offered as a mitigation.
- **No allowlist and no manual approval.** Open submission is how
  GWebCaches learn about each other (`docs/spec/`). An allowlist would close
  this cache to new caches, a change to discovery, not a fix.

## Consequences
- **What remains open is DNS rebinding toward clients.** A name that
  resolves to a public address when the cron pings it can later resolve to
  `192.168.x.y` for a Shareaza user, and that client then sends one GWC
  `GET` with a query string to a host on its own network.
  - **What bounds it:** Shareaza reads the reply as a cache's reply, and a
    reply that is not one counts as a failure; two failures remove the cache
    from that client. The cron re-pings verified caches every 6 hours and
    removes one after 3 failed pings, which bounds how long a cache that
    stops answering stays listed. It does not detect a name that answers the
    cron and rebinds only for clients.
  - **Accepted:** the same exposure exists for every GWebCache Shareaza
    knows. Removing it would need control over the resolution each client
    performs.
- **Rows stored before this rule are not deleted, but no longer listed.**
  Listings re-normalise every stored URL (`store::CacheRow::into_snap`), so
  a now-refused name drops out of replies. Its row still counts towards the
  cache cap and the per-host cap, and is still pinged, until three failed
  pings delete it or an operator removes it.
- **IPv6 is not covered.** Names such as `fe80--1.sslip.io` encode an IPv6
  address. Shareaza 2.7 resolves caches through WinINet, which may use AAAA
  records, so that remains an undocumented residual risk alongside
  rebinding.
