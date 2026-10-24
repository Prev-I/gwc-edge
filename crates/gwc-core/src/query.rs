//! Parsing of GWebCache v2 query strings.
//!
//! The query strings Shareaza actually sends are built in
//! `DiscoveryServices.cpp:1413-1434` (get) and `:1824-1870` (update); see
//! `docs/protocol.md` for the reconstruction this module implements.

/// Network named by `net=`. A missing or empty `net` means Gnutella, the
/// default network of GWC2 ("Extras: Using the Network Parameter"); only an
/// explicit `net=gnutella2` is served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Net {
    Gnutella2,
    /// Anything else, lowercased and truncated; answered politely, never served.
    Other(String),
}

/// The requesting client, from `client=` and, if present, `version=`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    /// Four-character vendor code, uppercased, e.g. `RAZA`.
    pub vendor: String,
    /// Version, sanitised to `[A-Za-z0-9._-]` and at most
    /// [`MAX_VERSION_LEN`] characters. May be empty.
    pub version: String,
}

/// Longest version string kept. Longer values are truncated, so that clients
/// cannot mint unbounded `clients_seen` rows.
pub const MAX_VERSION_LEN: usize = 16;

/// Longest `net` value kept.
const MAX_NET_LEN: usize = 16;

/// The network a query without `net` names.
const DEFAULT_NET: &str = "gnutella";

/// A parsed request. Several commands may be combined in one query: Shareaza
/// adds `ping=1` to every G2 get, and sends `ip=` and `url=` in one update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub client: Client,
    pub net: Net,
    pub ping: bool,
    /// Hosts were asked for.
    pub hostfile: bool,
    /// Caches were asked for.
    pub urlfile: bool,
    /// Raw `ip=` value, `<ip>:<port>`; validated by [`crate::addr`].
    pub ip: Option<String>,
    /// Decoded `url=` value; validated by [`crate::cache_url`].
    pub url: Option<String>,
    /// `x.leaves`: current leaf count reported by the hub.
    pub leaves: Option<u32>,
    /// `x.max`: leaf limit reported by the hub.
    pub max_leaves: Option<u32>,
    /// `uptime`: seconds the hub has been stably connected.
    pub uptime: Option<u32>,
}

impl Query {
    /// Whether this is an update (hub registration and/or cache submission).
    #[must_use]
    pub fn is_update(&self) -> bool {
        self.ip.is_some() || self.url.is_some()
    }
}

/// Why a query could not be parsed. Answered as `i|error|<reason>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryError {
    MissingClient,
    BadClient,
    /// No `ping`, `get`, `hostfile`, `urlfile`, `ip` or `url`.
    NoCommand,
    /// `update=1` with neither `ip=` nor `url=`.
    NothingToUpdate,
}

impl QueryError {
    /// Reason text for `i|error|<reason>`.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            QueryError::MissingClient => "client required",
            QueryError::BadClient => "client needs a 4-character vendor code",
            QueryError::NoCommand => "no command",
            QueryError::NothingToUpdate => "update needs ip or url",
        }
    }
}

/// Parse a raw query string, with or without its leading `?`.
///
/// # Errors
///
/// Returns a [`QueryError`] when the query names no usable client or command.
pub fn parse(raw: &str) -> Result<Query, QueryError> {
    let raw = raw.strip_prefix('?').unwrap_or(raw);
    let mut p = Params::default();
    for (key, value) in form_urlencoded::parse(raw.as_bytes()) {
        p.set(&key, value.into_owned());
    }

    let client = client(p.client.as_deref(), p.version.as_deref())?;

    let flag = |v: &Option<String>| v.as_deref().is_some_and(truthy);
    let (get, mut hostfile, mut urlfile) = (flag(&p.get), flag(&p.hostfile), flag(&p.urlfile));
    if get && p.hostfile.is_none() && p.urlfile.is_none() {
        // GWC v2: a get that names neither file asks for both.
        hostfile = true;
        urlfile = true;
    }
    let ping = flag(&p.ping);
    let nonempty = |v: Option<String>| v.filter(|v| !v.is_empty());
    let (ip, url) = (nonempty(p.ip), nonempty(p.url));

    if !(ping || hostfile || urlfile || ip.is_some() || url.is_some()) {
        return Err(if flag(&p.update) {
            QueryError::NothingToUpdate
        } else {
            QueryError::NoCommand
        });
    }

    let net = match p.net.as_deref().map(str::to_ascii_lowercase) {
        Some(n) if n == "gnutella2" => Net::Gnutella2,
        None => Net::Other(DEFAULT_NET.to_owned()),
        Some(n) if n.is_empty() => Net::Other(DEFAULT_NET.to_owned()),
        Some(n) => Net::Other(n.chars().take(MAX_NET_LEN).collect()),
    };

    let number = |v: Option<String>| v.and_then(|v| v.parse::<u32>().ok());
    Ok(Query {
        client,
        net,
        ping,
        hostfile,
        urlfile,
        ip,
        url,
        leaves: number(p.leaves),
        max_leaves: number(p.max_leaves),
        uptime: number(p.uptime),
    })
}

/// Raw parameters; the first occurrence of each wins.
#[derive(Default)]
struct Params {
    client: Option<String>,
    version: Option<String>,
    net: Option<String>,
    ping: Option<String>,
    get: Option<String>,
    hostfile: Option<String>,
    urlfile: Option<String>,
    update: Option<String>,
    ip: Option<String>,
    url: Option<String>,
    leaves: Option<String>,
    max_leaves: Option<String>,
    uptime: Option<String>,
}

impl Params {
    fn set(&mut self, key: &str, value: String) {
        let slot = match key {
            "client" => &mut self.client,
            "version" => &mut self.version,
            "net" => &mut self.net,
            "ping" => &mut self.ping,
            "get" => &mut self.get,
            "hostfile" => &mut self.hostfile,
            "urlfile" => &mut self.urlfile,
            "update" => &mut self.update,
            "ip" => &mut self.ip,
            "url" => &mut self.url,
            // Shareaza spells them with a dot (:1824); others use `_`.
            "x.leaves" | "x_leaves" => &mut self.leaves,
            "x.max" | "x_max" => &mut self.max_leaves,
            "uptime" => &mut self.uptime,
            _ => return,
        };
        if slot.is_none() {
            *slot = Some(value);
        }
    }
}

/// A flag is set unless its value is empty, `0`, `false`, `no` or `off`.
fn truthy(v: &str) -> bool {
    !(v.is_empty()
        || ["0", "false", "no", "off"]
            .iter()
            .any(|f| v.eq_ignore_ascii_case(f)))
}

/// Split `client=` into vendor and version. Shareaza glues the version to the
/// vendor code on G2 (`:1423-1424`, `:1862-1863`); `version=` wins if present.
fn client(client: Option<&str>, version: Option<&str>) -> Result<Client, QueryError> {
    let client = client
        .filter(|c| !c.is_empty())
        .ok_or(QueryError::MissingClient)?;
    let vendor = client.get(..4).ok_or(QueryError::BadClient)?;
    if !vendor.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(QueryError::BadClient);
    }
    let version = version.filter(|v| !v.is_empty()).unwrap_or(&client[4..]);
    Ok(Client {
        vendor: vendor.to_ascii_uppercase(),
        version: version
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            .take(MAX_VERSION_LEN)
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The four query shapes below are byte-for-byte what Shareaza 2.7.10.6
    // builds; only the IP, port, counters and URL are example values.

    /// G2 get for hosts: `DiscoveryServices.cpp:1415`, `:1422-1424`, `:1434`.
    /// The version is glued to the vendor code (`:1423-1424`), there is no
    /// `version=`, and `ping=1` always rides along.
    const SHAREAZA_G2_HOSTFILE: &str = "get=1&hostfile=1&net=gnutella2&ping=1&pv=4&client=RAZA2.7.10.6\
        &getleaves=1&getnetworks=1&getclusters=0&getvendors=1&getuptime=1";

    /// G2 get for caches: `:1413`, otherwise identical.
    const SHAREAZA_G2_URLFILE: &str = "get=1&urlfile=1&net=gnutella2&ping=1&pv=4&client=RAZA2.7.10.6\
        &getleaves=1&getnetworks=1&getclusters=0&getvendors=1&getuptime=1";

    /// G2 update from a hub that also submits another cache: `:1824-1830`,
    /// `:1834-1856` (URL-encoded), `:1858-1863`. Note the leading `?ip=`.
    const SHAREAZA_G2_UPDATE: &str = "?ip=203.0.113.7:6346&x.leaves=12&uptime=7300&x.max=300\
        &url=http%3A%2F%2Fgwc.example.org%2Fskulls.php&update=1&net=gnutella2&client=RAZA2.7.10.6";

    /// G1 get: `:1429-1431`. Spec-1 style `version=`, no `ping`.
    const SHAREAZA_G1_HOSTFILE: &str = "get=1&hostfile=1&net=gnutella&client=RAZA&version=2.7.10.6\
        &getleaves=1&getnetworks=1&getclusters=0&getvendors=1&getuptime=1";

    fn raza() -> Client {
        Client {
            vendor: "RAZA".into(),
            version: "2.7.10.6".into(),
        }
    }

    #[test]
    fn shareaza_g2_hostfile_query() {
        let q = parse(SHAREAZA_G2_HOSTFILE).unwrap();
        assert_eq!(q.client, raza());
        assert_eq!(q.net, Net::Gnutella2);
        assert!(q.ping, "Shareaza combines ping=1 with every G2 get");
        assert!(q.hostfile);
        assert!(!q.urlfile, "Shareaza asks for hosts and caches separately");
        assert!(!q.is_update());
    }

    #[test]
    fn shareaza_g2_urlfile_query() {
        let q = parse(SHAREAZA_G2_URLFILE).unwrap();
        assert_eq!(q.client, raza());
        assert!(q.ping);
        assert!(!q.hostfile);
        assert!(q.urlfile);
    }

    #[test]
    fn shareaza_g2_update_query_with_submitted_url() {
        let q = parse(SHAREAZA_G2_UPDATE).unwrap();
        assert_eq!(q.client, raza());
        assert_eq!(q.net, Net::Gnutella2);
        assert_eq!(q.ip.as_deref(), Some("203.0.113.7:6346"));
        assert_eq!(
            q.url.as_deref(),
            Some("http://gwc.example.org/skulls.php"),
            "url= is URL-encoded by Shareaza and must be decoded"
        );
        assert_eq!(q.leaves, Some(12));
        assert_eq!(q.max_leaves, Some(300));
        assert_eq!(q.uptime, Some(7300));
        assert!(q.is_update());
        assert!(!q.ping && !q.hostfile && !q.urlfile);
    }

    #[test]
    fn shareaza_submit_without_ip_is_an_update() {
        // wcmSubmit: `:1837` formats "%s?url=" when there is no ip= part.
        let q = parse(
            "?url=http%3A%2F%2Fgwc.example.org%2F&update=1&net=gnutella2&client=RAZA2.7.10.6",
        )
        .unwrap();
        assert!(q.is_update());
        assert_eq!(q.ip, None);
        assert_eq!(q.url.as_deref(), Some("http://gwc.example.org/"));
    }

    #[test]
    fn shareaza_g1_query_reads_version_parameter() {
        let q = parse(SHAREAZA_G1_HOSTFILE).unwrap();
        assert_eq!(q.client, raza());
        assert_eq!(q.net, Net::Other("gnutella".into()));
        assert!(!q.ping);
        assert!(q.hostfile);
    }

    #[test]
    fn task_spec_ping_shape() {
        let q = parse("ping=1&client=TEST&version=0.1").unwrap();
        assert!(q.ping);
        // GWC2 "Extras: Using the Network Parameter": Gnutella is the default.
        assert_eq!(
            q.net,
            Net::Other("gnutella".into()),
            "missing net= means gnutella"
        );
        assert_eq!(
            q.client,
            Client {
                vendor: "TEST".into(),
                version: "0.1".into()
            }
        );
    }

    #[test]
    fn version_parameter_wins_over_glued_version() {
        let q = parse("ping=1&client=RAZA9.9&version=2.7.10.6").unwrap();
        assert_eq!(q.client.version, "2.7.10.6");
    }

    #[test]
    fn vendor_is_uppercased_and_version_may_be_empty() {
        let q = parse("ping=1&client=raza").unwrap();
        assert_eq!(q.client.vendor, "RAZA");
        assert_eq!(q.client.version, "");
    }

    #[test]
    fn version_is_sanitised_and_truncated() {
        let q = parse("ping=1&client=RAZA2.7<script>10.6-beta_1.2345678901234567890").unwrap();
        assert_eq!(q.client.version, "2.7script10.6-be");
        assert_eq!(q.client.version.len(), MAX_VERSION_LEN);
    }

    #[test]
    fn net_is_case_insensitive_and_empty_means_gnutella() {
        assert_eq!(
            parse("ping=1&client=RAZA&net=GNUTELLA2").unwrap().net,
            Net::Gnutella2
        );
        assert_eq!(
            parse("ping=1&client=RAZA&net=").unwrap().net,
            Net::Other("gnutella".into())
        );
        assert_eq!(
            parse("ping=1&client=RAZA&net=Gnutella").unwrap().net,
            Net::Other("gnutella".into())
        );
    }

    #[test]
    fn get_without_file_selector_returns_both() {
        // GWC v2: a bare get=1 asks for hosts and caches.
        let q = parse("get=1&client=RAZA").unwrap();
        assert!(q.hostfile && q.urlfile);
    }

    #[test]
    fn hostfile_without_get_still_asks_for_hosts() {
        let q = parse("hostfile=1&client=RAZA").unwrap();
        assert!(q.hostfile && !q.urlfile);
    }

    #[test]
    fn ip_without_update_flag_is_still_an_update() {
        // GWC v1 updates carry no update=1; treat them the same.
        assert!(
            parse("ip=203.0.113.7:6346&client=RAZA")
                .unwrap()
                .is_update()
        );
    }

    #[test]
    fn zero_flag_is_false() {
        let q = parse("get=1&hostfile=1&urlfile=0&ping=0&client=RAZA").unwrap();
        assert!(q.hostfile && !q.urlfile && !q.ping);
        let q = parse("get=1&hostfile=1&ping=false&urlfile=No&client=RAZA").unwrap();
        assert!(!q.ping && !q.urlfile);
    }

    #[test]
    fn get_with_both_files_explicitly_off_asks_for_nothing() {
        assert_eq!(
            parse("get=1&hostfile=0&urlfile=0&client=RAZA"),
            Err(QueryError::NoCommand)
        );
    }

    #[test]
    fn first_occurrence_of_a_parameter_wins() {
        let q = parse("ping=1&client=RAZA&client=CRUX").unwrap();
        assert_eq!(q.client.vendor, "RAZA");
    }

    #[test]
    fn underscore_spelling_of_hub_counters_is_accepted() {
        let q = parse("ip=203.0.113.7:6346&x_leaves=5&x_max=200&client=RAZA").unwrap();
        assert_eq!((q.leaves, q.max_leaves), (Some(5), Some(200)));
    }

    #[test]
    fn unparsable_counters_are_dropped_not_fatal() {
        let q = parse("ip=203.0.113.7:6346&x.leaves=lots&uptime=-1&client=RAZA").unwrap();
        assert_eq!((q.leaves, q.uptime), (None, None));
    }

    #[test]
    fn missing_or_malformed_client_is_an_error() {
        assert_eq!(parse("ping=1"), Err(QueryError::MissingClient));
        assert_eq!(parse("ping=1&client="), Err(QueryError::MissingClient));
        assert_eq!(parse("ping=1&client=RA"), Err(QueryError::BadClient));
        assert_eq!(parse("ping=1&client=R%7CZA"), Err(QueryError::BadClient));
    }

    #[test]
    fn no_command_is_an_error() {
        assert_eq!(parse("client=RAZA"), Err(QueryError::NoCommand));
        assert_eq!(parse(""), Err(QueryError::MissingClient));
    }

    #[test]
    fn update_flag_alone_is_an_error() {
        assert_eq!(
            parse("update=1&client=RAZA"),
            Err(QueryError::NothingToUpdate)
        );
    }

    #[test]
    fn error_reasons_are_short_lowercase_and_pipe_free() {
        for e in [
            QueryError::MissingClient,
            QueryError::BadClient,
            QueryError::NoCommand,
            QueryError::NothingToUpdate,
        ] {
            let r = e.reason();
            assert!(!r.is_empty() && r.len() <= 40);
            assert!(!r.contains('|'));
            assert_eq!(r, r.to_lowercase());
        }
    }
}
