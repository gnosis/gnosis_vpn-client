/// Module for communicating with the Gnosis VPN root service over a Unix domain socket.
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use std::io;
use std::path::Path;

use crate::command::{Command, Response};

pub const DEFAULT_PATH: &str = "/var/run/gnosisvpn/gnosisvpn.sock";
pub const ENV_VAR: &str = "GNOSISVPN_SOCKET_PATH";

#[derive(Debug, Error)]
pub enum Error {
    #[error("service not running")]
    ServiceNotRunning,
    #[error("failed serializing command: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
}

pub async fn process_cmd(socket_path: &Path, cmd: &Command) -> Result<Response, Error> {
    check_path(socket_path)?;

    let mut stream = UnixStream::connect(socket_path).await?;

    let json_cmd = serde_json::to_string(cmd)?;
    push_command(&mut stream, &json_cmd).await?;
    let str_resp = pull_response(&mut stream).await?;
    serde_json::from_str::<Response>(&str_resp).map_err(Error::Serialization)
}

fn check_path(socket_path: &Path) -> Result<(), Error> {
    match socket_path.try_exists() {
        Ok(true) => Ok(()),
        Ok(false) => Err(Error::ServiceNotRunning),
        Err(x) => Err(x.into()),
    }
}

async fn push_command(socket: &mut UnixStream, json_cmd: &str) -> Result<(), Error> {
    // flush is not enough to push the command
    // we need to shutdown the write channel to signal the other side that all data was transferred
    socket.write_all(json_cmd.as_bytes()).await?;
    socket.flush().await?;
    socket.shutdown().await.map_err(Error::from)
}

async fn pull_response(socket: &mut UnixStream) -> Result<String, Error> {
    let mut response = String::new();
    socket
        .read_to_string(&mut response)
        .await
        .map(|_size| response)
        .map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;
    use tempfile::tempdir;

    fn sample_command() -> Command {
        Command::Ping
    }

    #[tokio::test]
    async fn check_path_reports_service_not_running_when_socket_missing() -> anyhow::Result<()> {
        let tmp = tempdir().expect("tempdir");
        let missing = tmp.path().join("missing.sock");
        let err = check_path(&missing).expect_err("missing socket should error");
        matches!(err, Error::ServiceNotRunning)
            .then_some(())
            .expect("service not running");
        Ok(())
    }

    #[tokio::test]
    async fn push_and_pull_round_trip_command_frames() -> anyhow::Result<()> {
        let (mut server, mut client) = UnixStream::pair().expect("pair");
        let json = serde_json::to_string(&sample_command()).expect("serialize");
        let push = push_command(&mut client, &json);
        let pull = pull_response(&mut server);
        tokio::try_join!(push, pull).expect("push and pull should complete round trip");
        Ok(())
    }

    #[tokio::test]
    async fn process_cmd_serializes_request_and_parses_response() -> anyhow::Result<()> {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("socket");
        let listener_path = path.clone();

        let server = tokio::spawn(async move {
            let listener = tokio::net::UnixListener::bind(&listener_path).expect("bind");
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = String::new();
                stream.read_to_string(&mut buf).await.expect("read");

                let cmd: Command = serde_json::from_str(&buf).expect("command");
                assert!(matches!(cmd, Command::Ping));

                let resp = Response::Pong;
                let json = serde_json::to_string(&resp).expect("json");

                stream.write_all(json.as_bytes()).await.expect("write response");
                stream.flush().await.expect("flush");
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let resp = process_cmd(path.as_path(), &sample_command()).await.expect("response");

        assert!(matches!(resp, Response::Pong));
        server.await.expect("listener task");
        Ok(())
    }
}
