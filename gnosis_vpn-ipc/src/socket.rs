//! Newline-delimited JSON over the service's Unix socket: one `Envelope` line in, one `Reply` line out.
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::protocol::*;
use crate::values::{DestinationId, FundingSecret};

#[cfg(target_os = "linux")]
pub const DEFAULT_PATH: &str = "/run/gnosisvpn/gnosisvpn.sock";
/// macOS keeps the flat `/var/run` name next to the pid file newsyslog reads.
#[cfg(target_os = "macos")]
pub const DEFAULT_PATH: &str = "/var/run/gnosisvpn.sock";
pub const ENV_VAR: &str = "GNOSISVPN_SOCKET_PATH";

/// A stuck service must not freeze a UI thread forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("service not running")]
    ServiceNotRunning,
    #[error("service closed the connection without a reply")]
    NoReply,
    #[error("unreadable reply: {0}")]
    Decode(serde_json::Error),
    #[error("reply does not match the request: {0:?}")]
    UnexpectedResponse(Box<Response>),
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

/// Sends one request on a fresh connection; prefer the typed functions below.
pub fn call(socket_path: &Path, request: Request) -> Result<Response, CallError> {
    if !socket_path.try_exists()? {
        return Err(CallError::ServiceNotRunning);
    }
    let stream = UnixStream::connect(socket_path)?;
    stream.set_read_timeout(Some(CALL_TIMEOUT))?;
    stream.set_write_timeout(Some(CALL_TIMEOUT))?;

    let envelope = Envelope {
        protocol: PROTOCOL_VERSION,
        request,
    };
    let mut line = serde_json::to_string(&envelope).expect("protocol types always serialize");
    line.push('\n');
    (&stream).write_all(line.as_bytes())?;

    let mut reply_line = String::new();
    BufReader::new(&stream).read_line(&mut reply_line)?;
    if reply_line.is_empty() {
        return Err(CallError::NoReply);
    }
    match serde_json::from_str::<Reply>(&reply_line).map_err(CallError::Decode)? {
        Reply::Ok { response } => Ok(response),
        Reply::Err { error } => Err(CallError::Service(error)),
    }
}

fn unexpected(response: Response) -> CallError {
    CallError::UnexpectedResponse(Box::new(response))
}

pub fn status(socket_path: &Path) -> Result<Status, CallError> {
    match call(socket_path, Request::Status)? {
        Response::Status(status) => Ok(*status),
        other => Err(unexpected(other)),
    }
}

pub fn nerd_stats(socket_path: &Path) -> Result<NerdStats, CallError> {
    match call(socket_path, Request::NerdStats)? {
        Response::NerdStats(stats) => Ok(*stats),
        other => Err(unexpected(other)),
    }
}

pub fn connect(socket_path: &Path, destination: DestinationId) -> Result<ConnectResponse, CallError> {
    match call(socket_path, Request::Connect { destination })? {
        Response::Connect(response) => Ok(response),
        other => Err(unexpected(other)),
    }
}

pub fn disconnect(socket_path: &Path) -> Result<DisconnectResponse, CallError> {
    match call(socket_path, Request::Disconnect)? {
        Response::Disconnect(response) => Ok(response),
        other => Err(unexpected(other)),
    }
}

pub fn balance(socket_path: &Path) -> Result<BalanceResponse, CallError> {
    match call(socket_path, Request::Balance)? {
        Response::Balance(response) => Ok(response),
        other => Err(unexpected(other)),
    }
}

pub fn funding_tool(socket_path: &Path, secret: FundingSecret) -> Result<FundingToolResponse, CallError> {
    match call(socket_path, Request::FundingTool { secret })? {
        Response::FundingTool(response) => Ok(response),
        other => Err(unexpected(other)),
    }
}

pub fn telemetry(socket_path: &Path) -> Result<Option<String>, CallError> {
    match call(socket_path, Request::Telemetry)? {
        Response::Telemetry(text) => Ok(text),
        other => Err(unexpected(other)),
    }
}

pub fn ping(socket_path: &Path) -> Result<(), CallError> {
    match call(socket_path, Request::Ping)? {
        Response::Pong => Ok(()),
        other => Err(unexpected(other)),
    }
}

pub fn info(socket_path: &Path) -> Result<InfoResponse, CallError> {
    match call(socket_path, Request::Info)? {
        Response::Info(info) => Ok(info),
        other => Err(unexpected(other)),
    }
}

pub fn start_client(socket_path: &Path, timeout: Duration) -> Result<StartClientResponse, CallError> {
    match call(socket_path, Request::StartClient { timeout })? {
        Response::StartClient(response) => Ok(response),
        other => Err(unexpected(other)),
    }
}

pub fn stop_client(socket_path: &Path) -> Result<StopClientResponse, CallError> {
    match call(socket_path, Request::StopClient)? {
        Response::StopClient(response) => Ok(response),
        other => Err(unexpected(other)),
    }
}

pub fn destinations(socket_path: &Path) -> Result<Vec<DestinationId>, CallError> {
    match call(socket_path, Request::Destinations)? {
        Response::Destinations(ids) => Ok(ids),
        other => Err(unexpected(other)),
    }
}

pub fn probe(socket_path: &Path, destination: DestinationId) -> Result<ProbeResponse, CallError> {
    match call(socket_path, Request::Probe { destination })? {
        Response::Probe(response) => Ok(response),
        other => Err(unexpected(other)),
    }
}

pub fn unprobe(socket_path: &Path) -> Result<UnprobeResponse, CallError> {
    match call(socket_path, Request::Unprobe)? {
        Response::Unprobe(response) => Ok(response),
        other => Err(unexpected(other)),
    }
}

pub fn quick_probe(socket_path: &Path, destination: DestinationId) -> Result<QuickProbeResponse, CallError> {
    match call(socket_path, Request::QuickProbe { destination })? {
        Response::QuickProbe(response) => Ok(response),
        other => Err(unexpected(other)),
    }
}

/// Service side: one request line, checked against this build's protocol version first.
pub fn decode_request(line: &str) -> Result<Request, ServiceError> {
    let invalid = |reason: String| ServiceError::InvalidRequest { reason };
    let raw: serde_json::Value = serde_json::from_str(line).map_err(|e| invalid(e.to_string()))?;
    let protocol = raw.get("protocol").and_then(serde_json::Value::as_u64);
    if protocol != Some(u64::from(PROTOCOL_VERSION)) {
        return Err(ServiceError::ProtocolMismatch {
            service: PROTOCOL_VERSION,
        });
    }
    let envelope: Envelope = serde_json::from_value(raw).map_err(|e| invalid(e.to_string()))?;
    Ok(envelope.request)
}

/// Service side: one reply line, newline included.
pub fn encode_reply(reply: &Reply) -> String {
    let mut line = serde_json::to_string(reply).expect("protocol types always serialize");
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::thread;

    fn serve_once(
        listener: UnixListener,
        reply: impl FnOnce(Result<Request, ServiceError>) -> Option<Reply> + Send + 'static,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).expect("read request");
            if let Some(reply) = reply(decode_request(&line)) {
                (&stream)
                    .write_all(encode_reply(&reply).as_bytes())
                    .expect("write reply");
            }
        })
    }

    #[test]
    fn a_request_gets_its_typed_response() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("socket");
        let server = serve_once(UnixListener::bind(&path).expect("bind"), |request| {
            assert_eq!(request, Ok(Request::Ping));
            Some(Reply::Ok {
                response: Response::Pong,
            })
        });

        ping(&path).expect("pong");
        server.join().expect("server");
    }

    #[test]
    fn a_service_error_reaches_the_caller() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("socket");
        let server = serve_once(UnixListener::bind(&path).expect("bind"), |_| {
            Some(Reply::Err {
                error: ServiceError::WorkerOffline,
            })
        });

        let err = status(&path).expect_err("worker offline");
        assert!(matches!(err, CallError::Service(ServiceError::WorkerOffline)));
        server.join().expect("server");
    }

    #[test]
    fn a_mismatched_response_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("socket");
        let server = serve_once(UnixListener::bind(&path).expect("bind"), |_| {
            Some(Reply::Ok {
                response: Response::Pong,
            })
        });

        let err = disconnect(&path).expect_err("pong is no disconnect response");
        assert!(matches!(err, CallError::UnexpectedResponse(_)));
        server.join().expect("server");
    }

    #[test]
    fn a_closed_connection_is_no_reply() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("socket");
        let server = serve_once(UnixListener::bind(&path).expect("bind"), |_| None);

        let err = ping(&path).expect_err("no reply");
        assert!(matches!(err, CallError::NoReply));
        server.join().expect("server");
    }

    #[test]
    fn a_missing_socket_means_the_service_is_not_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = ping(&dir.path().join("missing")).expect_err("no socket");
        assert!(matches!(err, CallError::ServiceNotRunning));
    }

    #[test]
    fn another_protocol_version_is_refused() {
        let other = r#"{"protocol":0,"request":{"type":"ping"}}"#;
        assert_eq!(
            decode_request(other),
            Err(ServiceError::ProtocolMismatch {
                service: PROTOCOL_VERSION
            })
        );
    }

    #[test]
    fn a_request_without_envelope_is_refused_as_mismatch() {
        assert_eq!(
            decode_request(r#""Status""#),
            Err(ServiceError::ProtocolMismatch {
                service: PROTOCOL_VERSION
            })
        );
    }

    #[test]
    fn an_unknown_request_is_invalid() {
        let unknown = format!(r#"{{"protocol":{PROTOCOL_VERSION},"request":{{"type":"reboot"}}}}"#);
        assert!(matches!(
            decode_request(&unknown),
            Err(ServiceError::InvalidRequest { .. })
        ));
    }
}
