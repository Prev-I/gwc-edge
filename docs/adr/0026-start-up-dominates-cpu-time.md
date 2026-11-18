# ADR-0026: Start-up dominates the Worker's CPU time; the module is kept small

**Status:** proposed, 2026-11-18. To be accepted once a production tail after
the deploy shows the effect. Qualifies the "10 ms CPU" budget in `AGENTS.md`.

## Context
Field test stage 5 (`docs/field-test-plan.md`) failed its CPU row. Over the
dashboard's last 7 days to 2026-11-18, CPU time per invocation was P50
11.5 ms, P90 31 ms, P99 61 ms, against the Free plan's 10 ms, with 0 errors in
about 14,920 invocations.

A 40-minute production tail on 2026-11-18 (54 invocations, `cpuTime` per
invocation next to the request log line) showed where the time goes:
- **The work does not explain it.** A `not_found` request, with no D1 access,
  cost 14 ms; `healthz` 13 ms; `update` 15 ms at the median. Gets served from
  the stored snapshot cost 17 ms at the median, 30 ms at P90.
- **A warm isolate is cheap.** Five gets read the stored snapshot for 1 to
  4 ms: the D1 read and the snapshot parse are not the cost.
- **Almost every request starts a fresh isolate.** Only 1 get in 39 found the
  snapshot in the isolate's memory (`snapshot: "memo"`), even 1 to 2 s after
  another get. Traffic is sparse and spread over many data centres, each with
  its own isolates.
- **The cron is over 10 ms on every run** (26 to 68 ms in the tail, up to
  about 110 ms on the dashboard), yet no run was cut short: every snapshot was
  rendered on time and no invocation ended `exceededCpu`.

Locally (Node's V8), compiling the 834 KB module cost about 4 to 6 ms of CPU
(an earlier, single run showed 10 to 13 ms), and instantiating it about 1 ms
more: about a third of the fresh-isolate cost. The rest (the JavaScript glue,
first calls into the module, the runtime) cannot be measured outside
Cloudflare.

Cloudflare's documentation states the Free limit (10 ms per HTTP request and
per Cron Trigger), a separate 1 s start-up limit for the global scope, and
"built-in flexibility" for a Worker that "infrequently runs over"; one that
"starts hitting the limit consistently" is terminated (error 1102,
`exceededCpu`). It does not say whether isolate start-up or module
compilation counts toward the 10 ms, or toward the `cpuTime` the dashboard
and logs show. The cron, consistently over 10 ms and never terminated,
suggests the enforced quantity is not the one shown, or that the tolerance
is wide; neither is documented.

The module was 853,858 bytes, 519 KB of it code: `js_sys` 31 %, `core` 18 %,
`gwc_worker` 12 %, `url` with `idna` and its Unicode tables about 10 %,
`serde_json` 5 %, `worker` 4 %, `gwc_core` 3.5 %. `url` came in only through
`req.url()`.

## Decision
- **The request URL is split, not parsed.** `gwc_core::request_url::split`
  reads the scheme, host, path and query from the runtime's serialised
  `Request.url`, and a test checks it against `url::Url` (a dev-dependency,
  never in the Worker). `url`, `idna` and the Unicode tables leave the module:
  853,858 to 667,062 bytes, code 519 to 449 KB, about 12 % less local compile
  time.
- **The module has a byte budget**, 720,000, checked in CI after the build
  (`scripts/wasm-size.sh`). Raising it is a deliberate change, made in the
  pull request that needs it, with the reason.
- **`opt-level` stays `"s"`.** `"z"` cut the code by 12 % but did not change
  the measured compile time.
- **No rewrite now.** Getting every request under 10 ms for certain would take
  a different architecture (for example a JavaScript Worker for the hot path).
  With no `exceededCpu` and no errors in production, that is not justified
  today.
- **`cpuTime` stays under watch:** an `exceededCpu` outcome, error 1102, or a
  rise in errors reopens this decision.

## Consequences
- **The "10 ms CPU" budget in `AGENTS.md` now means the request's own work.**
  Start-up is measured inside `cpuTime` and dominates it; it is reduced by
  keeping the module small, not by the request path.
- **New dependencies are weighed by their size in the module**, not only by
  what they do: the budget check makes growth visible in the pull request.
- **To be measured after the deploy:** a production tail of about 40 minutes,
  compared with the one above. This record is accepted with those figures.
- **What is not known:** how Cloudflare counts and enforces start-up CPU on
  the Free plan. Only Cloudflare can say; a question to them is still open.
