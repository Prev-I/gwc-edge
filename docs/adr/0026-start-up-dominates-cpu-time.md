# ADR-0026: Start-up dominates the Worker's CPU time; a smaller module did not measurably lower it

**Status:** accepted, 2026-11-18, after a production tail of the smaller
module (pull request #39). Qualifies the "10 ms CPU" budget in `AGENTS.md`.

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

Locally (Node's V8), compiling the 853,858-byte module cost about 4 to 6 ms
of CPU (an earlier, single run showed 10 to 13 ms), and instantiating it
about 1 ms more: about a third of the fresh-isolate cost. The rest (the JavaScript glue,
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

The module was 853,858 bytes, 519,211 of them function code: `js_sys` 31 %,
`core` 18 %, `gwc_worker` 12 %, `url` with `idna` and `icu_normalizer` about
10 % (about 49 KiB), `serde_json` 5 %, `worker` 4 %, `gwc_core` 3.5 %. The
rest of the file is data (among it the Unicode tables) and names. `url` came
in only through `req.url()`.

## Decision
- **The request URL is split, not parsed.** `gwc_core::request_url::split`
  reads the scheme, host, path and query from the runtime's serialised
  `Request.url`, and a test checks it against `url::Url` (a dev-dependency,
  never in the Worker). `url`, `idna` and the Unicode tables leave the module:
  853,858 to 667,062 bytes (−22 %); function code 519,211 to 449,242 bytes
  (−13.5 %), the rest of the 186,796 bytes being data and names. Local
  compile time fell from a median of 5.7 to 4.3 ms over 8 noisy runs each.
- **The module has a byte budget**, 720,000, checked in CI after the build
  (`scripts/wasm-size.sh`), so that it does not grow unnoticed. Raising it is
  a deliberate change, made in the pull request that needs it, with the
  reason.
- **No further size work for CPU's sake.** Measured in production, the
  smaller module had no measurable effect on the cost (see Consequences).
- **`opt-level` stays `"s"`.** `"z"` cut the code by 12 % but did not change
  the measured compile time.
- **No rewrite now.** Getting every request under 10 ms for certain would take
  a different architecture (for example a JavaScript Worker for the hot path).
  With no `exceededCpu` and no errors in production, that is not justified
  today.

## Consequences
- **The smaller module did not measurably lower production CPU.** A second 40-minute
  tail after the deploy (46 invocations, one version) measured requests at
  14, 16, 28 and 35 ms at P25, P50, P75 and P90, against 14, 17, 26 and 30 ms
  before (nearest-rank percentiles); gets from the stored snapshot at 16 ms
  (P50) and 32 ms (P90), against 17 and 30 ms; 6 of 42 requests at 10 ms or less, against 8 of 50;
  again 1 `memo` hit. The differences are within the noise of samples this
  size, so any saving from the smaller module is too small to see here.
  Together with the local measurement (compiling and instantiating about a
  third of a fresh isolate's cost), this suggests compilation is not the
  main part of the cost on Cloudflare. Possible reasons, none measurable
  from outside: compiled code may be reused across isolates, or the cost may
  lie in the JavaScript glue, the first calls into the module, or the
  runtime. One request ended `canceled` (the client most likely
  disconnected; wall 1.8 s), none `exceededCpu`.
- **The "10 ms CPU" budget in `AGENTS.md` means the request's own work.** The
  tails show a fresh isolate's start-up inside `cpuTime`, where it dominates
  (inferred from cold and warm requests, not documented by Cloudflare). It is
  not reduced by the request path, nor, measurably, by a smaller module.
- **The split URL and the byte budget stay.** They cost little, change no
  behaviour (a test checks the split against `url::Url`), and keep the module well inside the plan's size limit; the
  budget's purpose is that growth is seen and argued in the pull request, not
  a CPU saving.
- **What is not known:** how Cloudflare counts and enforces start-up CPU on
  the Free plan. Only Cloudflare can say; that question is still open.
- **What would reopen this:** an `exceededCpu` outcome, error 1102, or a rise
  in errors. The way out then is an architecture that does not pay a wasm
  isolate's start-up on the hot path (for example a JavaScript Worker for
  gets), a decision of its own.
