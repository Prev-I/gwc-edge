//! Thin Workers adapter: routing, D1 access and the cron entry point.
//! Protocol decisions live in `gwc_core`.

use serde::Serialize;
use worker::{Context, Env, Request, Response, Result, event};

/// D1 binding name, as declared in `wrangler.jsonc`.
const DB: &str = "DB";

#[derive(Serialize)]
struct Health {
    status: &'static str,
    product: &'static str,
    version: &'static str,
    database: &'static str,
}

#[event(start)]
fn start() {
    console_error_panic_hook::set_once();
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    match req.path().as_str() {
        "/healthz" => healthz(&env).await,
        _ => Response::error("Not Found", 404),
    }
}

/// Liveness plus a D1 round trip. `SELECT 1` scans no rows, so it costs no
/// rows-read quota.
async fn healthz(env: &Env) -> Result<Response> {
    let database = match env.d1(DB) {
        Ok(db) => match db.prepare("SELECT 1").run().await {
            Ok(_) => "ok",
            Err(_) => "error",
        },
        Err(_) => "unbound",
    };
    let health = Health {
        status: if database == "ok" { "ok" } else { "degraded" },
        product: gwc_core::PRODUCT,
        version: gwc_core::VERSION,
        database,
    };
    let status = if database == "ok" { 200 } else { 503 };
    Ok(Response::from_json(&health)?.with_status(status))
}
