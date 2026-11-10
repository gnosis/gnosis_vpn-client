use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use url::Url;

use std::fmt::{self, Display};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use gnosis_vpn_ipc::{ApiVersion, ExitVersions, Health};

use crate::remote_data;

#[derive(Clone, Serialize, Deserialize)]
pub struct Registration {
    public_key: String,
    ip: Ipv4Addr,
    newly_registered: bool,
    server_public_key: String,
    preshared_key: String,
}

/// The preshared key is a secret and `Up` is debug-logged with its registration.
impl fmt::Debug for Registration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registration")
            .field("public_key", &self.public_key)
            .field("ip", &self.ip)
            .field("newly_registered", &self.newly_registered)
            .field("server_public_key", &self.server_public_key)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct Input {
    public_key: String,
    socket_addr: SocketAddr,
    timeout: Duration,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Error parsing url: {0}")]
    Url(#[from] url::ParseError),
    #[error("Error making http request: {0:?}")]
    Request(#[from] reqwest::Error),
    #[error("Error connecting on specified port: {0:?}")]
    SocketConnect(reqwest::Error),
    #[error("Connection reset by peer: {0:?}")]
    ConnectionReset(reqwest::Error),
    #[error("Request timed out: {0:?}")]
    Timeout(reqwest::Error),
    #[error("Registration not found")]
    RegistrationNotFound,
}

/// What a connect-level failure says about the host, which decides whether another session listener
/// can help at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalConnectCause {
    /// The host could not assign a local address/port for the connection (macOS errno 49,
    /// `AddrNotAvailable`). A fresh listener port does not help: the state that broke the local
    /// address selection has to be rebuilt (the tunnel and its routes), then retried.
    AddressUnavailable,
    /// The listener refused the connection (`ConnectionRefused`): that particular session is gone,
    /// so a bridge session of our own does help.
    ListenerGone,
    /// No usable syscall detail: treat like a dead listener, the fallback is harmless there.
    Unknown,
}

#[cfg(test)]
pub(crate) use tests::closed_listener_error;

impl Error {
    /// A connect-level failure: the request never reached the exit's server because the host could
    /// not reach its own session listener. Every other variant means the request itself went wrong.
    pub fn is_local_connect(&self) -> bool {
        matches!(self, Error::SocketConnect(_))
    }

    /// Which kind of local connect failure this is; only meaningful when [`Self::is_local_connect`].
    pub fn local_cause(&self) -> LocalConnectCause {
        let Error::SocketConnect(err) = self else {
            return LocalConnectCause::Unknown;
        };
        let Some(io_error) = io_error_of(err) else {
            return LocalConnectCause::Unknown;
        };
        // `ErrorKind` is std's portable mapping of the platform errnos (macOS 49/61, Linux 99/111).
        // reqwest's `Display` hides the source chain, so matching on the message cannot work.
        cause_of_kind(io_error.kind()).unwrap_or(LocalConnectCause::Unknown)
    }
}

/// The connect failures whose handling differs, if this is one of them.
fn cause_of_kind(kind: std::io::ErrorKind) -> Option<LocalConnectCause> {
    match kind {
        // macOS: "Can't assign requested address".
        std::io::ErrorKind::AddrNotAvailable => Some(LocalConnectCause::AddressUnavailable),
        std::io::ErrorKind::ConnectionRefused => Some(LocalConnectCause::ListenerGone),
        _ => None,
    }
}

/// The first `io::Error` behind a reqwest failure, if the chain exposes one.
fn io_error_of(err: &reqwest::Error) -> Option<&std::io::Error> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(err);
    while let Some(current) = source {
        if let Some(io_error) = current.downcast_ref::<std::io::Error>() {
            return Some(io_error);
        }
        source = current.source();
    }
    None
}

impl Input {
    pub fn new(public_key: String, socket_addr: SocketAddr, timeout: Duration) -> Self {
        Input {
            public_key,
            socket_addr,
            timeout,
        }
    }
}

impl Registration {
    pub fn ip(&self) -> Ipv4Addr {
        self.ip
    }

    pub fn address(&self) -> String {
        format!("{}/32", self.ip)
    }

    pub fn server_public_key(&self) -> String {
        self.server_public_key.clone()
    }

    pub fn preshared_key(&self) -> String {
        self.preshared_key.clone()
    }
}

/// The only exit API this client speaks; every route below lives under it.
pub const API: ApiVersion = ApiVersion::new(1);

/// What `/versions` publishes; `latest` is ignored, as no decision depends on it.
#[derive(Deserialize)]
struct PublishedVersions {
    versions: Vec<String>,
    server: Option<semver::Version>,
}

pub async fn versions(client: &Client, socket_addr: SocketAddr, timeout: Duration) -> Result<ExitVersions, Error> {
    let headers = remote_data::json_headers();
    let url = Url::parse(
        format!(
            "http://{host}:{port}/versions",
            host = socket_addr.ip(),
            port = socket_addr.port()
        )
        .as_str(),
    )?;
    tracing::debug!(?headers, ?url, "get server versions");
    let resp = client
        .get(url)
        .timeout(timeout)
        .headers(headers)
        .send()
        .await
        .map_err(connect_errors)?
        .error_for_status()?
        .json::<PublishedVersions>()
        .await?;

    // Labels this client cannot parse are versions it does not speak, so they must not fail the check.
    let api = resp.versions.iter().filter_map(|label| label.parse().ok()).collect();
    Ok(ExitVersions {
        api,
        server: resp.server,
    })
}

pub async fn ping(client: &Client, socket_addr: SocketAddr, timeout: Duration) -> Result<(), Error> {
    let url = Url::parse(
        format!(
            "http://{host}:{port}{api}/ping",
            host = socket_addr.ip(),
            port = socket_addr.port(),
            api = API.path()
        )
        .as_str(),
    )?;
    tracing::debug!(?url, "ping exit server");
    client
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .map_err(connect_errors)?
        .error_for_status()?;

    Ok(())
}

pub async fn health(client: &Client, socket_addr: SocketAddr, timeout: Duration) -> Result<Health, Error> {
    let headers = remote_data::json_headers();
    let url = Url::parse(
        format!(
            "http://{host}:{port}{api}/status",
            host = socket_addr.ip(),
            port = socket_addr.port(),
            api = API.path()
        )
        .as_str(),
    )?;
    tracing::debug!(?headers, ?url, "get server health");
    let resp = client
        .get(url)
        .timeout(timeout)
        .headers(headers)
        .send()
        .await
        // connection error checks happen before response
        .map_err(connect_errors)?
        .error_for_status()?
        .json::<Health>()
        .await?;

    Ok(resp)
}

pub async fn register(client: &Client, input: &Input) -> Result<Registration, Error> {
    let headers = remote_data::json_headers();
    let url = Url::parse(
        format!(
            "http://{host}:{port}{api}/clients/register",
            host = input.socket_addr.ip(),
            port = input.socket_addr.port(),
            api = API.path()
        )
        .as_str(),
    )?;
    let json = json!({
        "public_key": input.public_key,
    });
    tracing::debug!(?headers, body = ?json, ?url, "post register client");
    let resp = client
        .post(url)
        .json(&json)
        .timeout(input.timeout)
        .headers(headers)
        .send()
        .await
        // connection error checks happen before response
        .map_err(connect_errors)?
        .error_for_status()?
        .json::<Registration>()
        .await?;

    Ok(resp)
}

pub async fn unregister(client: &Client, input: &Input) -> Result<(), Error> {
    let headers = remote_data::json_headers();
    let url = Url::parse(
        format!(
            "http://{host}:{port}{api}/clients/unregister",
            host = input.socket_addr.ip(),
            port = input.socket_addr.port(),
            api = API.path()
        )
        .as_str(),
    )?;
    let mut json = serde_json::Map::new();
    json.insert("public_key".to_string(), json!(input.public_key));
    tracing::debug!(?headers, body = ?json, ?url, "post unregister client");
    client
        .post(url)
        .json(&json)
        .timeout(input.timeout)
        .headers(headers)
        .send()
        .await
        // connection error checks happen before response
        .map_err(connect_errors)?
        .error_for_status()
        // response error checks happen after response
        .map_err(response_errors)?;

    Ok(())
}

fn connect_errors(err: reqwest::Error) -> Error {
    if err.is_connect() {
        Error::SocketConnect(err)
    } else if err.is_timeout() {
        // Before `is_request`, which also covers timeouts and would call them resets.
        Error::Timeout(err)
    } else if err.is_request() {
        Error::ConnectionReset(err)
    } else {
        err.into()
    }
}

fn response_errors(err: reqwest::Error) -> Error {
    if err.status() == Some(reqwest::StatusCode::NOT_FOUND) {
        Error::RegistrationNotFound
    } else {
        err.into()
    }
}

impl Display for Registration {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "WgRegistration {{ ip: {} }}", self.ip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn registration_debug_hides_the_preshared_key() {
        let registration: Registration = serde_json::from_value(json!({
            "public_key": "pub",
            "ip": "10.128.0.5",
            "newly_registered": false,
            "server_public_key": "srv",
            "preshared_key": "secret-psk",
        }))
        .expect("valid registration");
        let printed = format!("{registration:?}");
        assert!(printed.contains("pub"));
        assert!(!printed.contains("secret-psk"), "{printed}");
    }

    /// A listener that is gone refuses the connection: the session is dead, a fresh listener helps.
    #[tokio::test]
    async fn a_closed_listener_is_a_local_connect_failure_of_the_listener_gone_kind() {
        let err = closed_listener_error().await;

        assert!(err.is_local_connect(), "{err:?}");
        assert_eq!(err.local_cause(), LocalConnectCause::ListenerGone, "{err:?}");
    }

    /// A listener that accepts but never answers is a request failure, not a local connect one.
    #[tokio::test]
    async fn a_silent_listener_is_not_a_local_connect_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr");
        // Hold the accepted connection open; the response never comes. The request times out after
        // 300 ms; a full second leaves headroom on a loaded machine without slowing the suite much.
        let held = std::thread::spawn(move || {
            let _accepted = listener.accept().map(|(stream, _)| stream);
            std::thread::sleep(Duration::from_secs(1));
        });
        let input = Input::new("pub".to_string(), addr, Duration::from_millis(300));

        let err = register(&Client::new(), &input)
            .await
            .expect_err("silent listener times out");

        assert!(!err.is_local_connect(), "{err:?}");
        assert!(matches!(err, Error::Timeout(_)), "{err:?}");
        let _ = held.join();
    }

    /// The kind mapping is what separates "rebuild the tunnel" from "open another listener".
    #[test]
    fn local_cause_maps_the_error_kinds_we_can_name() {
        use std::io::ErrorKind;

        assert_eq!(
            cause_of_kind(ErrorKind::AddrNotAvailable),
            Some(LocalConnectCause::AddressUnavailable)
        );
        assert_eq!(
            cause_of_kind(ErrorKind::ConnectionRefused),
            Some(LocalConnectCause::ListenerGone)
        );
        // Anything else is not a case we act on differently.
        assert_eq!(cause_of_kind(ErrorKind::TimedOut), None);
        assert_eq!(cause_of_kind(ErrorKind::ConnectionReset), None);
    }

    /// A real `SocketConnect` from a request to a port nothing listens on, for other modules' tests.
    ///
    /// The port is free again before connecting, so a parallel test (or the connect's own ephemeral
    /// source port) can take it; retry until the kernel actually refuses.
    pub(crate) async fn closed_listener_error() -> Error {
        for _ in 0..16 {
            let port = {
                let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
                listener.local_addr().expect("local addr").port()
            };
            let input = Input::new(
                "pub".to_string(),
                format!("127.0.0.1:{port}").parse().unwrap(),
                Duration::from_secs(5),
            );
            let err = register(&Client::new(), &input)
                .await
                .expect_err("closed listener fails");
            if err.local_cause() == LocalConnectCause::ListenerGone {
                return err;
            }
        }
        panic!("no loopback connect was refused in 16 attempts");
    }
}
