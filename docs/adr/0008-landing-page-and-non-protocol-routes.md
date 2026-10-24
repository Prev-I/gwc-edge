# ADR-0008: Landing page, robots, methods, alias hosts

**Status:** accepted, 2026-10-23.

## Context
Shareaza always sends GWC parameters. Anything else is a person, a crawler or
another operator, and must neither cost D1 budget nor ever interfere with the
protocol path.

## Decision
- A request is a protocol request if and only if it carries one of `get`,
  `ping`, `update`, `ip`, `url`, `hostfile`, `urlfile`. Headers never decide
  protocol versus page; only `Host` decides whether a page request is
  redirected.
- `GET /` without them: a self-contained HTML page (inline CSS, no scripts, no
  external assets, under 8 KB), cached at the edge for 300 s, with aggregate
  numbers only: never a hub address. `/stats` is cached for 60 s.
- `/robots.txt` disallows everything. `HEAD` is answered like `GET` without a
  body; any other method gets 405 with an empty body.
- On a host other than the canonical one, `/` without parameters is a 301 to
  the canonical URL; with parameters it is the normal protocol reply, so the
  URL a client stores never diverges from the `u|` line the cache lists.

## Consequences
- On the custom domain, the page costs one D1 read of two rows per data centre
  per 5 minutes, and `/stats` per minute.
- The Cache API has no effect on `*.workers.dev`. `wrangler.jsonc` disables
  that address and preview URLs (`workers_dev: false`, `preview_urls: false`),
  so the Worker answers only on its custom domain.
- A page without every number (D1 down, before the first cron run, or a stored
  part this build cannot read) is never cached, so it reads two rows per
  request until the numbers exist. The cost is bounded at two rows either way.
