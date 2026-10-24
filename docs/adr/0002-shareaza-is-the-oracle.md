# ADR-0002: Shareaza is the protocol oracle; no reply may cost the cache its place

**Status:** accepted, 2026-10-23.

## Context
Shareaza 2.7 runs on nearly every G2 hub. It removes a cache from its list
after two failed replies, deleting the whole entry for G1 and G2 alike, and its
idea of failure is narrow (`docs/protocol.md`): an update reply without `OK`,
a get that adds no host or cache, an unrequested pong on a G1 get, any line
containing `ERROR`, `i|update|WARNING|bad url`.

## Decision
`docs/protocol.md`, reconstructed from Shareaza's source with citations, is the
protocol authority. Where a written specification disagrees with it, it wins,
and the deviation is recorded there. In particular:
- every update a legitimate client can send is answered
  `i|update|period|3600` then `i|update|OK`, including ones not stored;
  only denylisted clients get a warning;
- every hostfile or urlfile get ends with `u|<canonical URL>|0`, G1 included;
- rate-limited gets are answered like any other get, without data.

## Consequences
- Tests judge replies through `shareaza_model`, a model of Shareaza's parsers.
- Replies are less informative to other clients than a strict reading of the
  GWC spec would make them (no warnings for declined updates).
