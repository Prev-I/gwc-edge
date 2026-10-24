//! Thin Workers adapter: routing, D1 access, logging and the cron entry
//! point. Protocol decisions live in `gwc_core`.

mod cron;
mod gwc;
mod memo;
mod pages;
mod probe;
mod store;

use gwc_core::config::Config;
use gwc_core::counters::{self, Key, Kind, SAMPLE_K};
use gwc_core::health::Flush;
use gwc_core::log::{Outcome, RequestLine, route_kind};
use gwc_core::route::{self, Route};
use worker::{
    Context, Env, Headers, Method, Request, Response, Result, ScheduleContext, ScheduledEvent,
    console_error, console_log, console_warn, event,
};

use crate::probe::Probe;

/// The commit this build came from, set by the build command in
/// `wrangler.jsonc`.
pub const GIT_SHA: &str = match option_env!("GWC_GIT_SHA") {
    Some(sha) => sha,
    None => "unknown",
};

/// Current Unix time in seconds.
fn now() -> u64 {
    worker::Date::now().as_millis() / 1000
}

fn config(env: &Env) -> std::result::Result<Config, String> {
    Config::from_vars(|name| env.var(name).ok().map(|v| v.to_string()))
        .map_err(|e| format!("configuration error: {e:?}"))
}

#[event(start)]
fn start() {
    console_error_panic_hook::set_once();
}

/// What a request produced, for its one log line.
struct Done {
    resp: Response,
    kind: String,
    client: String,
    version: String,
    outcome: Outcome,
    hosts_sent: usize,
    caches_sent: usize,
    snapshot: Option<&'static str>,
    /// The sampled request counter to record, for GWC requests.
    count: Option<Key>,
}

impl Done {
    fn page(resp: Response, kind: &str, outcome: Outcome) -> Done {
        Done {
            resp,
            kind: kind.to_owned(),
            client: "-".into(),
            version: "-".into(),
            outcome,
            hosts_sent: 0,
            caches_sent: 0,
            snapshot: None,
            count: None,
        }
    }
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    let now = now();
    let url = req.url()?;
    let https = url.scheme() == "https";
    let method = match req.method() {
        Method::Get => route::Method::Get,
        Method::Head => route::Method::Head,
        _ => route::Method::Other,
    };
    let host = req
        .headers()
        .get("Host")?
        .unwrap_or_else(|| url.host_str().unwrap_or_default().to_owned());
    let colo = req.cf().map_or_else(|| "-".to_owned(), worker::Cf::colo);
    let mut probe = Probe::default();

    let done = match config(&env) {
        Ok(config) => {
            let route = route::classify(
                method,
                &host,
                url.path(),
                url.query().unwrap_or(""),
                &config.canonical_url,
            );
            // A handler error still gets a response and its one log line.
            match dispatch(route, &url, https, now, &config, &env, &mut probe).await {
                Ok(done) => done,
                Err(e) => internal_error(&e)?,
            }
        }
        Err(e) => {
            // Every route but /healthz is refused: there is no URL to list.
            let resp = if url.path() == "/healthz" {
                match pages::healthz(Err(&e), &env, now, &mut probe).await {
                    Ok((resp, _)) => resp,
                    Err(err) => internal_error(&err)?.resp,
                }
            } else {
                Response::error(e, 500)?
            };
            Done::page(resp, route_kind::CONFIG_ERROR, Outcome::Degraded)
        }
    };

    finish(done, method, https, now, &colo, &env, &ctx, &probe)
}

async fn dispatch(
    route: Route,
    url: &worker::Url,
    https: bool,
    now: u64,
    config: &Config,
    env: &Env,
    probe: &mut Probe,
) -> Result<Done> {
    Ok(match route {
        Route::Gwc => {
            let a = gwc::handle(url.query().unwrap_or(""), https, now, config, env, probe).await;
            Done {
                resp: text(&a.body)?,
                kind: a.key.kind.as_str().to_owned(),
                client: a.key.client.clone(),
                version: a.key.version.clone(),
                outcome: a.outcome,
                hosts_sent: a.hosts_sent,
                caches_sent: a.caches_sent,
                snapshot: a.snapshot,
                count: Some(a.key),
            }
        }
        Route::Landing => {
            let (resp, outcome) = pages::landing(config, env, probe).await?;
            Done::page(resp, route_kind::PAGE, outcome)
        }
        Route::Stats => {
            let (resp, outcome) = pages::stats(config, env, probe).await?;
            Done::page(resp, route_kind::STATS, outcome)
        }
        Route::Healthz => {
            let (resp, outcome) = pages::healthz(Ok(config), env, now, probe).await?;
            Done::page(resp, route_kind::HEALTHZ, outcome)
        }
        Route::Robots => Done::page(pages::robots()?, route_kind::ROBOTS, Outcome::Ok),
        Route::Redirect(to) => {
            let h = Headers::new();
            h.set("Location", &to)?;
            Done::page(
                Response::empty()?.with_status(301).with_headers(h),
                route_kind::REDIRECT,
                Outcome::Ok,
            )
        }
        Route::MethodNotAllowed => {
            let h = Headers::new();
            h.set("Allow", "GET, HEAD")?;
            Done::page(
                Response::empty()?.with_status(405).with_headers(h),
                route_kind::METHOD_NOT_ALLOWED,
                Outcome::Ok,
            )
        }
        Route::NotFound => Done::page(
            Response::error("Not Found", 404)?,
            route_kind::NOT_FOUND,
            Outcome::Ok,
        ),
    })
}

/// A handler failed (not D1: those are handled by the probe). Logged at
/// error level; the request's own line follows with kind `internal_error`.
fn internal_error(e: &worker::Error) -> Result<Done> {
    console_error!(
        "{}",
        serde_json::json!({"level": "error", "event": "internal_error", "error": e.to_string()})
    );
    Ok(Done::page(
        Response::error("Internal Error", 500)?,
        route_kind::INTERNAL_ERROR,
        Outcome::Degraded,
    ))
}

/// Log the request's one line, record the sampled counter and flush the
/// health counters (not in degraded mode: no further D1 access), and drop
/// the body of a HEAD response.
#[allow(clippy::too_many_arguments)]
fn finish(
    done: Done,
    method: route::Method,
    https: bool,
    now: u64,
    colo: &str,
    env: &Env,
    ctx: &Context,
    probe: &Probe,
) -> Result<Response> {
    let line = RequestLine {
        kind: &done.kind,
        client: &done.client,
        version: &done.version,
        scheme: if https { "https" } else { "http" },
        outcome: done.outcome,
        hosts_sent: done.hosts_sent,
        caches_sent: done.caches_sent,
        d1_ms: probe.d1_ms,
        cf_colo: colo,
        snapshot: done.snapshot,
    };
    console_log!("{}", line.to_json());
    if done.outcome == Outcome::Degraded {
        memo::with_health(gwc_core::health::HealthCounters::degraded);
    }
    // No further D1 access from a degraded request, whether D1 failed in
    // this request or the reply came from a copy remembered after a failure.
    if !probe.failed() && done.outcome != Outcome::Degraded {
        let sampled = done.count.filter(|_| counters::sampled(random_u32()));
        let flush = memo::with_health(|h| h.take_due(now));
        if sampled.is_some() || flush.is_some() {
            let env = env.clone();
            ctx.wait_until(record(env, now, sampled, flush));
        }
    }
    if method == route::Method::Head {
        let headers = done.resp.headers().clone();
        let status = done.resp.status_code();
        return Ok(Response::empty()?.with_status(status).with_headers(headers));
    }
    Ok(done.resp)
}

/// Uniform `u32` from `Math.random()`; `counters::sampled` reads the high bits.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn random_u32() -> u32 {
    (worker::js_sys::Math::random() * 4_294_967_296.0) as u32
}

/// After the response: the sampled request counter and any due health
/// counts. Best-effort; health counts are given back if their write fails.
async fn record(env: Env, now: u64, sampled: Option<Key>, flush: Option<Flush>) {
    let hour = counters::hour(now);
    let Ok(db) = store::db(&env) else {
        if let Some(f) = flush {
            memo::with_health(|h| h.restore(f));
        }
        return;
    };
    if let Some(key) = sampled {
        match store::count(&db, hour, &key, SAMPLE_K).await {
            Ok(Some(1)) => {}
            // Budget sizing assumes 1 row per counter write; say so if not.
            Ok(rows) => console_warn!(
                "{}",
                serde_json::json!({"level": "warn", "event": "count_rows_written", "rows": format!("{rows:?}")})
            ),
            Err(e) => console_error!("{}", gwc_core::log::d1_error_line("count", &e.to_string())),
        }
    }
    if let Some(f) = flush {
        // One batch: both counts land, or neither does and both are restored.
        let weight = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
        let counts: Vec<(Key, u32)> = [(Kind::D1Error, f.d1_errors), (Kind::Degraded, f.degraded)]
            .into_iter()
            .filter(|&(_, n)| n > 0)
            .map(|(kind, n)| (Key::new(kind, None, false), weight(n)))
            .collect();
        if let Err(e) = store::count_batch(&db, hour, &counts).await {
            console_error!(
                "{}",
                gwc_core::log::d1_error_line("health_flush", &e.to_string())
            );
            memo::with_health(|h| h.restore(f));
        }
    }
}

/// A `text/plain` GWebCache body. `no-transform` stops compression, which
/// Shareaza does not decode on this path (`HttpRequest.cpp:169-186`).
fn text(body: &str) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", "text/plain; charset=utf-8")?;
    headers.set("Cache-Control", "no-store, no-transform")?;
    Ok(Response::ok(body)?.with_headers(headers))
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    if let Err(e) = cron::run(&env).await {
        console_error!("{}", gwc_core::log::d1_error_line("cron", &e.to_string()));
        console_log!(
            "{}",
            serde_json::json!({"kind": "cron", "event": "cron_failed", "error": e.to_string()})
        );
    }
}
