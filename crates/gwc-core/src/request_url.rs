//! The parts of a request URL the Worker routes on, without the `url` crate.
//!
//! The runtime hands the Worker an already serialised absolute URL
//! (`Request.url`), so splitting it is enough; parsing it again with `url`
//! would link `idna` and its Unicode tables into the wasm, against the
//! module's byte budget (ADR-0026). The result matches `url::Url`'s `scheme()`,
//! `host_str()`, `path()` and `query()` for such URLs; the tests check it.

/// The routing parts of a serialised absolute URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestUrl<'a> {
    /// The scheme, without `://`.
    pub scheme: &'a str,
    /// The host, without userinfo or port; an IPv6 host keeps its brackets.
    pub host: &'a str,
    /// The path, `/` when the URL has none.
    pub path: &'a str,
    /// The query without its `?`; `Some("")` for a bare `?`.
    pub query: Option<&'a str>,
}

/// Split a serialised absolute URL such as `http://host/path?query`.
#[must_use]
pub fn split(url: &str) -> RequestUrl<'_> {
    // A fragment never reaches the server; drop it before anything else.
    let url = url.split_once('#').map_or(url, |(before, _)| before);
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, rest) = rest.split_at(authority_end);
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    RequestUrl {
        scheme,
        host: host_of(authority),
        path: if path.is_empty() { "/" } else { path },
        query,
    }
}

/// `[userinfo@]host[:port]` to `host`; an IPv6 literal keeps its brackets.
fn host_of(authority: &str) -> &str {
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if host_port.starts_with('[') {
        host_port
            .find(']')
            .map_or(host_port, |end| &host_port[..=end])
    } else {
        host_port
            .split_once(':')
            .map_or(host_port, |(host, _)| host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(u: &str) -> (&str, &str, &str, Option<&str>) {
        let r = split(u);
        (r.scheme, r.host, r.path, r.query)
    }

    #[test]
    fn a_shareaza_get() {
        assert_eq!(
            parts("http://gwc.example.org/?get=1&hostfile=1&net=gnutella2&client=RAZA2.7.10.2"),
            (
                "http",
                "gwc.example.org",
                "/",
                Some("get=1&hostfile=1&net=gnutella2&client=RAZA2.7.10.2")
            )
        );
    }

    #[test]
    fn a_page_without_query() {
        assert_eq!(
            parts("https://gwc.example.org/healthz"),
            ("https", "gwc.example.org", "/healthz", None)
        );
    }

    #[test]
    fn no_path_is_the_root() {
        assert_eq!(
            parts("http://gwc.example.org"),
            ("http", "gwc.example.org", "/", None)
        );
        assert_eq!(
            parts("http://gwc.example.org?a=1"),
            ("http", "gwc.example.org", "/", Some("a=1"))
        );
    }

    #[test]
    fn a_bare_question_mark_is_an_empty_query() {
        assert_eq!(
            parts("http://gwc.example.org/?"),
            ("http", "gwc.example.org", "/", Some(""))
        );
    }

    #[test]
    fn a_fragment_is_dropped() {
        assert_eq!(
            parts("http://gwc.example.org/x?a=1#top"),
            ("http", "gwc.example.org", "/x", Some("a=1"))
        );
        assert_eq!(
            parts("http://gwc.example.org/x#top?a=1"),
            ("http", "gwc.example.org", "/x", None)
        );
    }

    #[test]
    fn the_host_loses_userinfo_and_port() {
        assert_eq!(
            parts("http://u:p@gwc.example.org:8080/x").1,
            "gwc.example.org"
        );
        assert_eq!(parts("http://[::1]:8787/x").1, "[::1]");
        assert_eq!(parts("http://127.0.0.1:18787/?ping=1").1, "127.0.0.1");
    }

    /// The same answers as `url::Url` for the URLs the runtime serialises.
    #[test]
    fn matches_the_url_crate() {
        let urls = [
            "http://gwc.example.org/?get=1&hostfile=1&net=gnutella2&ping=1&pv=4&client=RAZA2.7.10.2&getleaves=1&getnetworks=1&getclusters=0&getvendors=1&getuptime=1",
            "http://gwc.example.org/?ip=1.2.3.4:6346&x.leaves=10&uptime=100&x.max=300&url=http%3A%2F%2Fother.example.net%2F&update=1&net=gnutella2&client=RAZI2.7.11.4",
            "http://gwc.example.org/?url=http%3A%2F%2Fother.example.net%2F&update=1&net=gnutella2&client=RAZA2.7.10.6",
            "http://gwc.example.org/?get=1&urlfile=1&net=gnutella&client=RAZA&version=2.7.10.6",
            "http://gwc.example.org/?ping=1&client=TEST&version=gwc-edge%200.5.0",
            "http://gwc.example.org/?support=1",
            "http://gwc.example.org/",
            "http://gwc.example.org",
            "http://gwc.example.org/?",
            "https://gwc.example.org/stats",
            "https://gwc.example.org/healthz",
            "http://gwc.example.org/robots.txt",
            "http://gwc.example.org/no/such/page?x=%20y",
            "http://alias.example.org/",
            "http://127.0.0.1:18787/?ping=1&client=GWCE&version=smoke",
            "http://localhost:8787/?get=1&hostfile=1",
            "http://[::1]:8787/healthz",
            "http://u:p@gwc.example.org:8080/a/b?c=d#e",
            "http://gwc.example.org#f",
            "http://gwc.example.org/?#f",
            "http://gwc.example.org/a%3F%23b?c",
            "http://gwc.example.org?u=/x",
            "HTTP://GWC.Example.ORG:80/Healthz?A=B",
            "http://bücher.example/?q=ü",
        ];
        for raw in urls {
            // What the runtime hands the Worker: the serialised form.
            let want = url::Url::parse(raw).unwrap();
            let u = want.as_str();
            let r = split(u);
            assert_eq!(r.scheme, want.scheme(), "{u}");
            assert_eq!(r.host, want.host_str().unwrap_or_default(), "{u}");
            assert_eq!(r.path, want.path(), "{u}");
            assert_eq!(r.query, want.query(), "{u}");
        }
    }

    proptest::proptest! {
        /// Any URL the `url` crate can serialise splits into its parts.
        #[test]
        fn any_serialised_url_matches_the_url_crate(
            raw in "(?i:https?)://([a-z0-9.-]{1,20}|\\[[0-9a-f:]{2,12}\\])(:[0-9]{1,5})?(/[ -~]{0,30})?(\\?[ -~]{0,30})?(#[ -~]{0,10})?"
        ) {
            if let Ok(want) = url::Url::parse(&raw)
                && let Some(host) = want.host_str()
            {
                let r = split(want.as_str());
                proptest::prop_assert_eq!(r.scheme, want.scheme());
                proptest::prop_assert_eq!(r.host, host);
                proptest::prop_assert_eq!(r.path, want.path());
                proptest::prop_assert_eq!(r.query, want.query());
            }
        }
    }
}
