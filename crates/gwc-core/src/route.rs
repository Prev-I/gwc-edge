//! Which handler a request goes to.
//!
//! Shareaza never sends a request without GWC parameters, so a request with
//! none of them is a person, a crawler or another cache operator: it gets the
//! landing page. The discrimination is on parameters only, never on headers
//! such as `Accept`, which Shareaza does not send.

use crate::cache_url::CacheUrl;

/// The query parameters that make a request a GWebCache request.
pub const GWC_PARAMS: [&str; 7] = ["get", "ping", "update", "ip", "url", "hostfile", "urlfile"];

/// The request method, as far as routing cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    /// Answered like `GET`, without a body.
    Head,
    Other,
}

/// Where a request goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The GWebCache protocol.
    Gwc,
    /// The HTML landing page.
    Landing,
    /// `301` to this URL: the landing page on the canonical host.
    Redirect(String),
    Robots,
    Stats,
    Healthz,
    /// `405`, empty body.
    MethodNotAllowed,
    NotFound,
}

/// Whether `query` carries any GWC parameter (empty values included).
#[must_use]
pub fn has_gwc_params(query: &str) -> bool {
    form_urlencoded::parse(query.as_bytes()).any(|(k, _)| GWC_PARAMS.contains(&k.as_ref()))
}

/// Route a request. `host` is the `Host` header (with any port), `path` and
/// `query` are as received (query without `?`).
///
/// - Any method but `GET` and `HEAD` → [`Route::MethodNotAllowed`].
/// - `/robots.txt`, `/stats`, `/healthz` → their handlers.
/// - `/` or the canonical path (case-insensitive) with GWC parameters →
///   [`Route::Gwc`], on any host: a redirect there would make the URL a
///   client stores diverge from the `u|` line this cache lists for itself.
/// - The same paths without GWC parameters → [`Route::Landing`] on the
///   canonical host, [`Route::Redirect`] to the canonical URL on any other.
/// - Anything else → [`Route::NotFound`].
#[must_use]
pub fn classify(
    method: Method,
    host: &str,
    path: &str,
    query: &str,
    canonical: &CacheUrl,
) -> Route {
    if method == Method::Other {
        return Route::MethodNotAllowed;
    }
    match path {
        "/robots.txt" => return Route::Robots,
        "/stats" => return Route::Stats,
        "/healthz" => return Route::Healthz,
        _ => {}
    }
    let (canonical_authority, canonical_path) = split(&canonical.url);
    if path != "/" && !path.eq_ignore_ascii_case(canonical_path) {
        return Route::NotFound;
    }
    if has_gwc_params(query) {
        Route::Gwc
    } else if same_authority(host, canonical_authority) {
        Route::Landing
    } else {
        Route::Redirect(canonical.url.clone())
    }
}

/// `http://authority/path` → (`authority`, `/path`).
fn split(url: &str) -> (&str, &str) {
    let rest = url.strip_prefix("http://").unwrap_or(url);
    rest.find('/').map_or((rest, "/"), |i| rest.split_at(i))
}

/// Case-insensitive, with `:80` equal to no port.
fn same_authority(host: &str, canonical: &str) -> bool {
    let strip = |h: &str| h.strip_suffix(":80").unwrap_or(h).to_ascii_lowercase();
    strip(host) == strip(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_url::normalize;

    fn canonical() -> CacheUrl {
        normalize("http://gwc.example.org/").unwrap()
    }

    fn route(method: Method, host: &str, path: &str, query: &str) -> Route {
        classify(method, host, path, query, &canonical())
    }

    const HOST: &str = "gwc.example.org";
    const SHAREAZA: &str = "get=1&hostfile=1&net=gnutella2&ping=1&pv=4&client=RAZA2.7.10.6";

    #[test]
    fn no_gwc_params_is_landing() {
        assert_eq!(route(Method::Get, HOST, "/", ""), Route::Landing);
        assert_eq!(
            route(Method::Get, HOST, "/", "client=RAZA&net=gnutella2"),
            Route::Landing
        );
        assert_eq!(
            route(Method::Get, HOST, "/", "utm_source=x"),
            Route::Landing
        );
    }

    #[test]
    fn any_of_the_seven_params_is_gwc() {
        for p in GWC_PARAMS {
            assert_eq!(
                route(Method::Get, HOST, "/", &format!("{p}=1")),
                Route::Gwc,
                "{p}"
            );
            assert_eq!(
                route(Method::Get, HOST, "/", &format!("x=1&{p}=")),
                Route::Gwc,
                "{p} empty"
            );
        }
        assert_eq!(route(Method::Get, HOST, "/", SHAREAZA), Route::Gwc);
    }

    #[test]
    fn head_is_routed_like_get() {
        assert_eq!(route(Method::Head, HOST, "/", ""), Route::Landing);
        assert_eq!(route(Method::Head, HOST, "/", SHAREAZA), Route::Gwc);
        assert_eq!(route(Method::Head, HOST, "/stats", ""), Route::Stats);
    }

    #[test]
    fn other_methods_are_405() {
        for path in ["/", "/stats", "/robots.txt", "/nope"] {
            assert_eq!(
                route(Method::Other, HOST, path, SHAREAZA),
                Route::MethodNotAllowed
            );
        }
    }

    #[test]
    fn fixed_paths() {
        assert_eq!(route(Method::Get, HOST, "/robots.txt", ""), Route::Robots);
        assert_eq!(route(Method::Get, HOST, "/stats", ""), Route::Stats);
        assert_eq!(route(Method::Get, HOST, "/healthz", ""), Route::Healthz);
        assert_eq!(
            route(Method::Get, "alias.example.net", "/stats", ""),
            Route::Stats
        );
    }

    #[test]
    fn secondary_host_root_redirects_to_canonical() {
        assert_eq!(
            route(Method::Get, "alias.example.net", "/", ""),
            Route::Redirect("http://gwc.example.org/".into())
        );
        assert_eq!(
            route(Method::Get, "gwc-edge.example.workers.dev", "/", ""),
            Route::Redirect("http://gwc.example.org/".into())
        );
    }

    #[test]
    fn secondary_host_with_gwc_params_is_gwc() {
        assert_eq!(
            route(Method::Get, "alias.example.net", "/", SHAREAZA),
            Route::Gwc
        );
    }

    #[test]
    fn host_comparison_ignores_case_and_default_port() {
        assert_eq!(
            route(Method::Get, "GWC.Example.ORG", "/", ""),
            Route::Landing
        );
        assert_eq!(
            route(Method::Get, "gwc.example.org:80", "/", ""),
            Route::Landing
        );
        assert_ne!(
            route(Method::Get, "gwc.example.org:8080", "/", ""),
            Route::Landing
        );
    }

    #[test]
    fn canonical_path_is_matched_case_insensitively() {
        let c = normalize("http://gwc.example.org/GWC/skulls.php").unwrap();
        let r = |path, q| classify(Method::Get, HOST, path, q, &c);
        assert_eq!(r("/gwc/SKULLS.PHP", SHAREAZA), Route::Gwc);
        assert_eq!(r("/GWC/skulls.php", ""), Route::Landing);
        assert_eq!(r("/", SHAREAZA), Route::Gwc, "the root always answers");
    }

    #[test]
    fn unknown_path_is_404() {
        assert_eq!(
            route(Method::Get, HOST, "/favicon.ico", ""),
            Route::NotFound
        );
        assert_eq!(
            route(Method::Get, HOST, "/wp-admin", SHAREAZA),
            Route::NotFound
        );
    }
}
