# GWebCache v2 as Shareaza speaks it

This is the protocol `gwc-edge` implements: GWebCache v2 ("GWC2", pipe-separated
responses), reconstructed from what Shareaza 2.7 actually sends and accepts.
The written specification is vague in places, and Shareaza runs on nearly every
G2 hub (about 97% per the project brief, from <http://crawler.doxu.org/>). So
where the two differ, Shareaza wins. Every such case is listed in
[Deviations from the written spec](#deviations-from-the-written-spec).

## Sources

**Oracle:** <https://github.com/ansani/Shareaza> at commit
`f24b0ce3753067e02fb6ee7dbf82d8c214c291d2`, files under `shareaza/`. Citations
below use short names:

| Short | File |
|---|---|
| `DS` | `DiscoveryServices.cpp` |
| `HR` | `HttpRequest.cpp` |
| `SN` | `ShakeNeighbour.cpp` |
| `HC` | `HostCache.cpp` |
| `Set` | `Settings.cpp` |
| `Sec` | `Security.cpp` |

`DS:1423` means line 1423 of `DiscoveryServices.cpp` at that commit. In the
code, a bare `:N` means `DiscoveryServices.cpp` unless the module names another
file.

**How far the oracle is from the clients in the field:**
- `DiscoveryServices.cpp` was last changed on 2017-03-27, when it was synced to
  upstream SVN r9660. That is the 2.7.10 line, so its parser is the one deployed
  hubs run.
- The fork differs elsewhere:
  - It reports vendor `RAZI` (`Shareaza.h:444`), where upstream reports `RAZA`.
  - Its User-Agent is `iShareaza <version>` (`Shareaza.cpp:1109-1111`), where
    upstream sends `Shareaza <version>`.
  - Nothing here depends on either value.

**Other references:**
- The live cache `dkac` (<http://dkac.trillinux.org/dkac/dkac.php>) was probed
  once, read-only, with `?ping=1&client=TEST&version=0.1`.
- It answered `200 OK` with `Content-Type: text/plain; charset=UTF-8` and the
  body `ERROR No network\n`. So it requires `net=`, and ends lines with LF.

## Transport

Shareaza fetches a cache with WinINet, through `CHttpRequest`.

| Behaviour | Detail | Source |
|---|---|---|
| Method | `GET`. The URL is the stored cache address plus a query string. | `DS:1948-1960` |
| Request headers | None are added. User-Agent is the WinINet agent, `<client name> <version>`. Everything else is WinINet's default. | `Network.cpp:238-242` |
| Redirects | Followed. WinINet's default; `INTERNET_FLAG_NO_AUTO_REDIRECT` is not set. | `HR:253-257` |
| TLS | WinINet could do it (`HR:86` accepts `https`), but a cache learned from a `u\|` line must be `http://` (see below). | |
| Status codes | Any non-2xx is a failure. The body is not parsed. | `HR:142-145`, `DS:1438-1439`, `DS:1875-1876` |
| Reason phrase | If the status reason phrase is empty, the body is skipped and the request fails, even on `200`. **Always send a reason phrase.** | `HR:262-265` |
| Body size | No limit on the GWC path. | `HR:40`; `LimitContentLength` not called |
| Encoding | The body is decoded as UTF-8, with no charset negotiation and no BOM handling. | `HttpRequest.h:49`, `HR:158-161` |
| Compression | gzip and deflate are **not** decoded on this path. `InflateResponse` is only called for BitTorrent trackers. | `HR:169-186`, `BTTrackerRequest.cpp:534` |

What follows for `gwc-edge`:
- Always answer HTTP `200 OK`, including for errors.
- Send `text/plain` in ASCII.
- Set `Cache-Control: no-store, no-transform`, so neither Cloudflare nor a proxy
  compresses or caches the body.

## Requests Shareaza sends

Shareaza builds exactly four request shapes.

**G2 get** (`DS:1413-1434`) asks for hosts *or* caches, never both. `ping=1` is
always included:

```
<cache>?get=1&hostfile=1&net=gnutella2&ping=1&pv=4&client=RAZA2.7.10.6&getleaves=1&getnetworks=1&getclusters=0&getvendors=1&getuptime=1
<cache>?get=1&urlfile=1&net=gnutella2&ping=1&pv=4&client=RAZA2.7.10.6&getleaves=1&getnetworks=1&getclusters=0&getvendors=1&getuptime=1
```

**G1 get** (`DS:1427-1431`) has no `ping`, and uses spec-1 `version=`:

```
<cache>?get=1&hostfile=1&net=gnutella&client=RAZA&version=2.7.10.6&getleaves=1&...
```

**Update** (`DS:1824-1870`) comes from a hub. It may also submit another cache
URL, which is URL-encoded and truncated at the first non-ASCII character
(`DS:1845-1853`):

```
<cache>?ip=<a.b.c.d>:<port>&x.leaves=<n>&uptime=<s>&x.max=<n>&url=<encoded>&update=1&net=gnutella2&client=RAZA2.7.10.6
```

**Submit** (`DS:1837-1841`) is a cache URL alone:

```
<cache>?url=<encoded>&update=1&net=gnutella2&client=RAZA2.7.10.6
```

Things to note:
- **On G2 the version is glued to the vendor code**, as in
  `client=RAZA2.7.10.6`, with no `version=` (`DS:1423-1424`, `DS:1862-1863`).
  The vendor code is the first four characters.
- `x.leaves` and `x.max` are written with a dot. Other clients use `x_leaves`
  and `x_max`.
- `ip=` is Shareaza's own idea of its address (`Network.m_pHost`). Behind NAT
  this can be a private address.
- **The submitted `url=` is a *third* cache.** In a hub update it is picked at
  random from the hub's own list of working caches (`DS:1298`, `DS:1833-1856`),
  not the cache being updated.
- **Submit mode** is started by a user (`WndDiscovery.cpp:361`). The user picks
  the cache to submit, and the target is a random cache that need not be a
  working one (`DS:1306-1308`). Any client can send it, not only hubs.
- A hub running both G1 and G2 **alternates** which network it updates
  (`DS:872-881`).
- `pv=4`, `getnetworks` and `getclusters` are sent but have no effect on what
  Shareaza accepts back.

## When Shareaza talks to a cache

- **Queries:**
  - A given cache is queried at most every `AccessPeriod`, 30 min by default
    (`Set:346`, `DS:1194`).
  - The cache can change that period with `i|access|period|<s>` (`DS:1670-1683`).
    The value is saved with the cache (`DS:2180`), so a non-default value sticks.
  - The 60 s gate between queries (`DS:943`) applies only to normal discovery.
    Forced discovery skips it.
- **Follow-up URL query:**
  - **Right after a successful host query, Shareaza sends a urlfile query to the
    same cache.** It does this when it is on its first run, or knows fewer than
    `Lowpoint` (10) caches (`DS:1340-1352`).
  - That second request comes milliseconds after the first, from the same
    address, and skips both gates above.
  - The follow-up's result replaces the first one's (`DS:1351`). So if the
    follow-up fails, the whole exchange counts as a failure.
  - A `u|` line in the host reply clears `m_bFirstTime` (`DS:1791-1792`) before
    the check at `DS:1346`. On a first run this skips the follow-up, which is
    harmless.
- **Updates** carrying `ip=` are sent only by hubs (`DS:859-910`). All of these
  must hold:
  - G2 hub mode is on.
  - The hub has been stable for at least 2 h.
  - It has at least 4 neighbours.
  - No update was sent in the last `DefaultUpdate`, 1 h (`Set:343`, `DS:863`).
- **Updates go only to working caches:** ones with at least one access and zero
  failures (`DS:1192`).
- **Per-cache update period:** `m_nUpdatePeriod` defaults to 1 h (`DS:2139`).
  The cache can change it with `i|update|period|<s>` (`DS:1927-1934`).

## Reading a get response

Parsing is in `CDiscoveryServices::RunWebCacheGet` (`DS:1399-1803`).

**Splitting lines and fields:**
- The body is split on `\r` or `\n`, and each line is trimmed of
  `\r\n \t`. Empty lines are skipped (`DS:1453-1460`).
- Each line is split on `|`, and each part is trimmed (`DS:1462-1469`).
- Empty middle fields survive, so `a||b` has three parts and `a||` has two.
  A single trailing `|` adds nothing, so `a|` has one.

Lines are matched by their first field, case-insensitively, in this order:

| Line | Effect | Source |
|---|---|---|
| `h\|ip:port\|age[\|cluster\|leaves\|vendor\|uptime\|leaflimit]` | Adds a host. Fewer than 3 fields, a bad address or a bad age **fails the whole response**. | `DS:1472-1566` |
| ↳ `ip:port` | Parsed by `inet_addr`, which also accepts short, octal and hex forms. The `:` must be past position 6. Port 1..65535. | `DS:1476-1482` |
| ↳ `age` | An integer, clamped to 0..1 year. | `DS:1487` |
| ↳ `leaves`, `leaflimit` | Each empty or an integer in 0..2047, **else the whole response fails**. | `DS:1491-1503`, `DS:1533-1546` |
| ↳ `vendor` | First 4 characters. A blocked vendor **fails the whole response**. The only blocked vendor is any containing `foxy`. | `DS:1506-1515`, `Sec:1313-1321` |
| ↳ `uptime` | Empty, or 61..one year minus a second, **else the whole response fails**. | `DS:1518-1531` |
| `u\|url\|age` | Adds a cache if the URL starts with `http://`. Also adds `ukhl:` URLs on G2, and `uhc:` URLs on G1. Anything else is ignored silently. Fewer than 2 fields fails. | `DS:1568-1593`, `DiscoveryServices.h:29-30` |
| `UHC\|…` / `UKHL\|…` | UDP host caches for G1 and G2 respectively. | `DS:1594-1627` |
| `i\|pong\|<name>\|<nets>` | On a G1 query, the response **fails** ("mystery pong"). On G2, the response fails if `<nets>` (split on `-`) is present and lacks `gnutella2`. | `DS:1636-1669` |
| `i\|access\|period\|<s>` | Sets this cache's access period. | `DS:1670-1683` |
| `i\|force\|remove` | **Removes the cache.** | `DS:1684-1693` |
| `i\|update\|warning\|bad url` | **Removes the cache.** | `DS:1695-1705` |
| `i\|networks\|…`, `i\|nets\|…` | The response fails if `gnutella2` is not listed. | `DS:1706-1749` |
| any other `i\|…` | Ignored, including `i\|error\|…` and `i\|update\|…`. | |
| `PONG …` | Ignored on G2; on G1 the response fails. | `DS:1750-1757` |
| a line containing `ERROR` | The response fails, unless the line also contains `Something Failed`. | `DS:1759-1770` |
| a plain IP or plain URL | Accepted as G1 data, **and the cache is reflagged G1-only**. | `DS:1771-1785` |
| anything else | The response fails. | `DS:1787-1788` |

A get succeeds only if **at least one host or cache was added** (`DS:1794-1802`).
Two details make that rule easier to meet:
- A `u|` line counts even when Shareaza already knows the URL. `Add` checks
  `GetByAddress` on the trimmed string and returns `TRUE` (`DS:138-140`), before
  any validation of the original string.
- A host line counts when `HostCache.Add` accepts it.

So an empty response, or one with only `i|` lines, is a failure.

## Reading an update response

Parsing is in `RunWebCacheUpdate` (`DS:1808-1942`). Lines are split and trimmed
the same way, and examined in order:

1. A line containing `OK` (**case-sensitive**) means success. Shareaza
   **stops reading** there (`DS:1898-1904`).
2. A line containing `i|warning|client|early` or `You came back too early`
   means failure (`DS:1905-1909`).
3. A line containing `ERROR` (case-insensitive) means failure (`DS:1910-1918`).
4. `i|access|period|<s>` and `i|update|period|<s>` set the periods
   (`DS:1919-1934`).
5. A line that is exactly `i|force|remove` removes the cache (`DS:1935-1939`).

If no line matched rule 1, the update **fails** (`DS:1942`). This includes every
`i|update|WARNING|…` reply.

## Failures and removal

- A failed get or update calls `OnFailure` (`DS:1376-1391`).
- At `Discovery.FailureLimit` failures (default **2**, `Set:344`) the cache is
  removed from the client's list (`DS:2408-2418`).
- **Removal deletes the whole entry, for G1 and G2 alike** (`DS:2153-2156`).
  There is no per-network removal.
- A success, or any added host, resets the count (`DS:2375-2380`, `DS:2396-2399`).
- Separately, a cache with one or more failures stops receiving updates until it
  succeeds again (`DS:1192`).
- **Network flags:** a cache added without a protocol, as a user's
  `Discovery → Add` does, is flagged G1 *and* G2 (`DS:231-234`, `DS:247-253`).
  Shareaza then sends it G1 gets as well, whenever G1 is enabled.

So two bad answers in a row to the same client lose that client, on every
network.

## Cache URL validity

`CDiscoveryServices::Add` (`DS:117-238`) and `CheckWebCacheValid` (`DS:297-345`):

- **Clean-up:** the URL is trimmed, then trailing `.` and `/` characters are
  stripped, and one `/` is restored if there was one (`DS:127-132`).
- **Rejected when any of these fail:**
  - the original string is at least 12 characters (`DS:300`);
  - the scheme is `http://` or `https://` (`DS:320-333`);
  - a `.` appears somewhere after the scheme, with a `/` somewhere after that
    dot (`DS:336-341`).
- **Not checked:** query strings, ports, IP literals (including WinINet's
  short, octal and hex forms), maximum length.
- **Comparing long URLs:** addresses longer than 45 characters are compared on
  all but their last 2 characters (`DS:377-381`).
- **Query strings:** Shareaza appends `?get=…` to the stored URL (`DS:1413`).
  A URL that already contains `?` therefore cannot work.

## G2 handshake (hub verification)

A Shareaza hub accepting an incoming connection (`SN:636-702`, `SN:925-1257`):

- **Opening line:** it accepts any line starting `GNUTELLA CONNECT/` that is
  longer than 17 characters (`SN:684-687`). `/0.4` is closed without a reply
  (`SN:678-682`).
- **Headers it reads:**
  - `Accept: application/x-gnutella2` (or `application/x-shareaza`) selects G2
    (`SN:826-830`).
  - Without either, the hub assumes G1 (`SN:937`). If G1 is disabled it replies
    `503 Wrong Protocol` (`SN:1028-1046`).
  - `X-Ultrapeer`, or its alias `X-Hub`, marks the peer a hub only when its value
    is `True`. Any other value means leaf (`SN:796-800`). A missing header also
    means leaf (`SN:1018-1022`).
  - A `User-Agent` that is present but empty is blocked (`Sec:1297-1300`).
- **Accepting:** it replies `GNUTELLA/0.6 200 OK` with `User-Agent`, `Accept`
  and `Content-Type: application/x-gnutella2`, `X-Ultrapeer: True`,
  `X-Ultrapeer-Needed`, `Listen-IP` and `Remote-IP: a.b.c.d:port`. There is no
  `X-Try-Hubs` on a 200 (`SN:1245-1257`, `SN:317-438`).
- **Refusing:** it replies `GNUTELLA/0.6 503 <reason>`, then the minimal headers,
  then `X-Try-Hubs` (`SN:544-629`). The reasons are:

  | Reason | Meaning | Source |
  |---|---|---|
  | `Maximum connections reached` | Hub, full. Also sent to non-Shareaza leaves when the hub is near its limit (`SN:1223`), so it is over-represented. | `SN:1220-1226` |
  | `Shielded leaf node` | Not a hub: the peer is a leaf. | `SN:1190` |
  | `Need an Ultrapeer` | Not a hub: the peer is in leaf mode. | `SN:1238-1241` |
  | `Ultrapeer disabled` | Not a hub. | `SN:1230-1234` |
  | `Wrong Protocol` | Not G2. Its status line already ends in `\r\n` and another is added, so the header block is empty. | `SN:1044` |

- **Banned peers** get no status line at all: the connection is just closed
  (`DelayClose`, `SN:1024-1026` for a banned User-Agent).
- **`X-Try-Hubs`:**
  - Entries are `a.b.c.d:port YYYY-MM-DDThh:mmZ`, joined by `,` with no space
    (`SN:578-591`, `HC:1862-1865`).
  - A hub sends up to `Gnutella2.HostCount` entries: 15 by default, capped at 50
    (`Set:394`).
  - When reading an entry, Shareaza parses the timestamp with `TimeFromString`
    (`Shareaza.cpp:1851-1889`), which takes only the minute form. An entry whose
    timestamp is present but unparsable is dropped (`HC:385-401`).
- **Header lines:**
  - Lines end at `\n`, and a trailing `\r` is trimmed (`Buffer.cpp:343-374`).
  - Bytes are decoded as UTF-8, falling back to the ANSI code page
    (`Buffer.cpp:362`, `Strings.cpp:234-258`).
  - A header name is 2..63 characters (`Connection.cpp:1171-1172`).
  - A line starting with whitespace is re-delivered under the last header name
    (`Connection.cpp:1154-1169`). For `X-Try-Hubs` that *replaces* the stored
    value (`SN:903`).
- **Leaving after the reply:**
  - Closing the connection after reading the reply only logs a dropped
    connection on the hub (`SN:267-278`).
  - The handshake times out after 45 s (`Set:303`).

## What gwc-edge emits

**Which requests are protocol requests** (`route::classify`):
- A request is a GWC request if and only if its query carries one of `get`,
  `ping`, `update`, `ip`, `url`, `hostfile` or `urlfile`, even with an empty
  value. Headers are never consulted: Shareaza sends no `Accept`.
- A GWC request is answered on `/` and on the canonical path, on **any** host.
  A redirect there would make the URL a client stores diverge from the `u|` line
  this cache lists for itself.
- Without GWC parameters, `/` is the HTML landing page on the canonical host,
  and a 301 to the canonical URL on any other host. `/stats`, `/healthz` and
  `/robots.txt` are fixed paths. `HEAD` is answered like `GET` without a body;
  any other method gets 405 with an empty body. See ADR-0008.

**Every GWC response:**
- HTTP `200 OK`, `Content-Type: text/plain; charset=utf-8`,
  `Cache-Control: no-store, no-transform`.
- The body is ASCII, with every line ending in `\n`, the last one included.
- Lines start only with `h|`, `u|` or `i|`.

**This cache's own URL:**
- It is one canonical `http://` URL: the `CANONICAL_URL` variable, validated by
  `cache_url::normalize` (the Worker answers 500 without a valid one). It is
  served identically everywhere.
- It must match the address clients stored, case-insensitively, for the
  "known URL" path at `DS:138` to apply.
- An alias such as `*.workers.dev` beside the custom domain would break this.

**Gets** (`response::get_reply`):
- **Every hostfile or urlfile request ends with `u|<this cache's URL>|0`.** That
  includes G1 and rate-limited requests. The line makes Shareaza count the reply
  as a success even with nothing else in it (`DS:138-140`, `DS:1794-1802`).
- **G2:**
  - **Pong:** `i|pong|gwc-edge <version>|gnutella2`, sent only when `ping=1` was
    given.
  - **Hosts:** `h|ip:port|age` lines, newest first, at most 1 per /24 and 4
    per /16 in one reply (ADR-0011).
    - Extended fields are filled only when they pass Shareaza's checks: leaves
      and leaf limit 0..2047, uptime 61 s..1 year, vendor 4 alphanumerics and not
      `foxy`.
    - A field that fails is left empty, and trailing empty fields are dropped.
  - **Caches:** `u|url|age` lines.
- **Any other network:** only the self line. There are no hosts (we have none
  for G1), and no pong, which a G1 query treats as a failure (`DS:1641-1646`).
- **A ping without a get** gets the pong on any network, so no allowed client
  ever receives an empty body. Shareaza never sends one: its G1 queries carry
  no ping, and its G2 pings always come with a get.
- **Degraded mode** (D1 failed, ADR-0007): the same replies, built from no
  data, or from hubs the isolate still remembers.
- **Rate-limited gets:** answered as above with no hosts and no caches, and no
  D1 read. That means the pong if it was asked for, plus the self line.
- **Rate-limit keys:** the source address and the request type (hostfile,
  urlfile or update), with IPv6 grouped by /64. Shareaza's back-to-back hostfile
  and urlfile requests therefore never limit each other.
- **Denylisted clients:** an empty body. Shareaza counts it as a failure and,
  after two, drops this cache, which is the intent.

**Updates** (`response::update_reply`):
- **Everything a legitimate client can send** gets the same reply:

  ```
  i|update|period|3600
  i|update|OK
  ```

  - That covers accepted, merged and rate-limited updates, and ones we decline
    to store: a NAT or private address, a port below 1024, a third-party cache
    URL we do not keep, a G1 update from a dual-mode hub.
  - Each part of a combined `ip=` + `url=` update is judged separately (see
    "What gwc-edge stores"). The reply is the same either way.
  - The period line comes first, because Shareaza stops reading at `OK`.
  - 3600 is Shareaza's own default. Each hub is gated globally to one update an
    hour anyway (`DS:863`), so a lower value would only raise this cache's share.
- **Denylisted clients** get one `i|update|WARNING|client not supported` line.
  - It contains none of `OK`, `error`, `bad url` or `early`.
  - Shareaza counts it as a failure, but it never removes the cache on the spot.

**Malformed queries:**
- The reply is `i|error|<reason>`.
- Shareaza never sends one, so this only reaches other clients.

**Cache URLs** (`cache_url::normalize`):
- Clean-up follows Shareaza's own.
- Accepted URLs:
  - `http://` only;
  - the host is a dotted DNS name, or a canonical public IPv4 address. A host
    that looks numeric must be a canonical dotted quad, so `127.1`, `0177.0.0.1`
    and `0x7f.0.0.1` are rejected;
  - the port is 1..65535, written without leading zeros, and `:80` is dropped;
  - no `?`, `#`, `@`, `|`, `\`, whitespace or non-ASCII;
  - 12..255 characters.
- A missing path becomes `/`.
- `https://` URLs are recognised but not stored. Shareaza ignores them in `u|`
  lines.
- DNS names that *resolve* to private addresses cannot be caught here. Cache
  verification must cover that.

**Hub addresses** (`addr::parse_hub`):
- Canonical dotted-quad IPv4 only, so no leading zeros, which `inet_addr` would
  read as octal.
- The address must be public unicast.
- The port must be 1024..65535.

**Hub verification** (`handshake`):
- **Request:** `GNUTELLA CONNECT/0.6` with `User-Agent: gwc-edge <version>`,
  `Accept: application/x-gnutella2` and `X-Ultrapeer: False`. After reading the
  reply, the connection is closed.
- **Verdict:**

  | Reply | Status |
  |---|---|
  | `200` with a G2 content type, and `X-Ultrapeer`/`X-Hub` absent or `True` | `verified` |
  | `503` other than the not-a-hub reasons | `alive_full` |
  | anything else, including a connection closed before the header block ends | `dead` |

- **`X-Try-Hubs`:**
  - Read the way Shareaza reads it, with two differences.
  - We also accept `:ss` in timestamps, and we drop impossible dates rather than
    normalising them.
  - Continuation lines are appended rather than replacing the value.
  - Entries must pass the hub-address rules. At most 50 are kept.

## What gwc-edge stores

Every update is answered the same way (above); what it stores is decided part
by part (`gwc_core::update`, ADR-0010):

| Part | Stored when | As | Never stored |
|---|---|---|---|
| `ip=` | Gnutella2, a public IPv4 with port 1024..65535, **and the request comes from that address** (`CF-Connecting-IP`) | an `unverified` hub, source `update`, listed after the cron's first successful handshake (ADR-0011) | from any other address: not even as `unverified`; private, reserved or CGNAT addresses; low ports; G1 |
| `url=` | Gnutella2, a valid `http://` URL (see "Cache URLs" above), not this cache, fewer than 3 already for its host, fewer than 50 in all | a `pending` cache, not served until the cron has pinged it | `https://`, this cache's own URL, invalid URLs, G1 |

- Hubs enter `hubs` with a source other than their own address only as
  `X-Try-Hubs` candidates found by the verifying cron: source `try_hubs`,
  status `unverified`, not served until a handshake succeeds.
- At most 500 hubs and 50 caches. Of the hubs, at most 150 may be
  `X-Try-Hubs` candidates awaiting a handshake; self-registered hubs awaiting
  one count only against the 500.
- A hub that reaches the cache over IPv6 while claiming its IPv4 is not
  stored: the addresses differ (ADR-0010).
- `url=` on this cache's own host and port is never stored, whatever the path:
  every path there reaches this cache.
- A hub's row is written at most once per 30 minutes by its own updates,
  whatever its status or port. After that, a listed hub on the same port is
  refreshed; on a new port it returns to `unverified`; an `unverified` or
  `dead` hub is queued again for the cron. Only the cron lists a hub.
- Rate limits: 2 gets per 60 s per source and kind (hostfile and urlfile
  separately), 1 update per 60 s per source; over the limit the reply is the
  same, without data or storage.

## Deviations from the written spec

The task specification and Shareaza disagree in the places below. The table
gives what `gwc-edge` does. "Approved" means the project owner decided it.

| # | Written spec | Shareaza | gwc-edge | Status |
|---|---|---|---|---|
| 1 | Every client sends `client=XXXX` and `version=`. | G2 requests glue the version onto `client` and send no `version=` (`DS:1423`, `DS:1862`). | Vendor = first 4 characters of `client`. Version = `version=` if present, else the rest of `client`. | resolved |
| 2 | `ping` and `get` are separate requests; hosts and caches can be asked together. | Every G2 get carries `ping=1`, and hosts and caches are always separate requests (`DS:1413-1422`). | Commands combine freely: pong, then hosts, then caches. | resolved |
| 3 | Rate-limited update → `i\|update\|WARNING\|too many requests`; invalid update → WARNING. | Any update reply without `OK` is a failure, and 2 failures remove the cache (`DS:1898`, `DS:1942`, `Set:344`). Hubs send third-party URLs and alternate G1/G2 updates (`DS:1298`, `DS:872-881`). | `i\|update\|period\|3600` + `i\|update\|OK` for everything except denylisted clients: accepted, merged, rate-limited and declined updates alike. | **approved** (decision A, extended) |
| 4 | Denylisted client, empty cache, G1 → empty hostfile. | A get that adds nothing is a failure (`DS:1794-1802`), and removal deletes the G2 entry too (`DS:2153-2156`). | Every hostfile/urlfile get ends with the cache's own `u\|` line, G1 included. Only denylisted clients get an empty body. | **approved** (decision B, and the G1 follow-up) |
| 5 | The update period hint was proposed as 600 s. | `m_nUpdatePeriod` defaults to 3600 s (`DS:2139`, `Set:343`). | 3600. | resolved (corrects our own proposal) |
| 6 | `i\|update\|WARNING\|<reason>` with any reason. | `bad url` removes the cache (`DS:1700`); `OK` reads as success; `ERROR` and `early` read as failures. | The one remaining reason, `client not supported`, avoids all four. | resolved |
| 7 | A hub that answers `503` is `alive_full`. | `503 Shielded leaf node`, `Need an Ultrapeer`, `Ultrapeer disabled` and `Wrong Protocol` mean "not a hub" (`SN:1044`, `SN:1190`, `SN:1230-1241`). | Those four reasons give `dead`. Any other 503 gives `alive_full`. | resolved |
| 8 | `200` + `application/x-gnutella2` means verified. | Shareaza also takes `application/x-shareaza`, and treats any `X-Ultrapeer`/`X-Hub` value other than `True` as a leaf (`SN:796-800`, `SN:837-839`). | The same rules. | resolved |
| 9 | Cache URLs are any URL. | `u\|` lines are used only when the URL starts with `http://` (`DS:1573`). URLs with `?` cannot work (`DS:1413`). WinINet resolves numeric short forms to LAN addresses. | Only canonical `http://` URLs are stored. Anything declined is still answered `OK` (#3). | resolved |
| 10 | Unknown or malformed query → `i\|error\|<reason>`, HTTP 200. | Non-2xx is a failure (`HR:142-145`), and an empty reason phrase fails even a 200 (`HR:262-265`). | HTTP `200 OK` always. | consistent |
| 11 | Rate-limited get → `i\|update\|WARNING\|too many requests`, 1 get / 30 s per IP. | That reply adds nothing, so it is a failure. A single client sends hostfile and urlfile back to back (`DS:1340-1352`). | Limited per (address, request type), IPv6 by /64. Over the limit, the reply is the pong if asked plus the self `u\|` line, with no D1 read. | **approved** |
| 12 | Hubs register with their IP. | A hub behind NAT may report a private address (`Network.m_pHost`). | Not stored, answered `OK` (#3). | **approved** (with #3) |
| 13 | Never an empty body for an allowed client (brief addendum). | A pong on a G1 get is a failure (`DS:1641-1646`). | A ping without a get gets a pong on any network; G1 gets still get none. | **approved** |

## Test coverage of these findings

Each finding above is a unit test in `crates/gwc-core`, citing the line it
encodes:
- `query` holds the four request shapes and flag parsing. A flag is false
  when empty or `0`/`false`/`no`/`off`. A `get` naming neither `hostfile` nor
  `urlfile` asks for both, but naming one, even as `=0`, turns that default
  off. Shareaza never sends either edge case;
- `response` holds line formats and field bounds, and judges every get and
  update reply through `shareaza_model`, a test-only model of Shareaza's two
  parsers;
- `policy` holds the denylist and the rate-limit keys;
- `cache_url` holds URL validity and normalisation;
- `addr` holds hub address rules;
- `handshake` holds the handshake request, reply parsing, the 503 reasons,
  `X-Ultrapeer`/`X-Hub` and `X-Try-Hubs`.

`tests/no_panic.rs` property-tests every parser against arbitrary input.
