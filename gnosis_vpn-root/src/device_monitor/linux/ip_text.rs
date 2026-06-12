use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::device_monitor::NetworkEvent;

pub fn start(tx: mpsc::Sender<NetworkEvent>) -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(run(tx, cancel.clone()));
    (cancel, handle)
}

async fn run(tx: mpsc::Sender<NetworkEvent>, cancel: CancellationToken) {
    use tokio::io::AsyncBufReadExt;

    tracing::debug!("device monitor started (ip monitor text)");
    let child = tokio::process::Command::new("ip")
        .args(["monitor", "link", "address", "route"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = ?e, "device monitor: failed to spawn ip monitor");
            return;
        }
    };

    let stdout = child.stdout.take().expect("stdout was piped");
    let mut lines = tokio::io::BufReader::new(stdout).lines();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return;
            }
            line = lines.next_line() => match line {
                Err(e) => {
                    tracing::error!(error = ?e, "device monitor: ip monitor read error");
                    return;
                }
                Ok(None) => {
                    tracing::warn!("device monitor: ip monitor exited unexpectedly");
                    return;
                }
                Ok(Some(line)) => {
                    // Indented lines are continuations of the previous event (e.g. link/ether).
                    if !line.is_empty() && !line.starts_with(char::is_whitespace) {
                        let event = parse_event(&line);
                        if tx.try_send(event).is_err() {
                            tracing::warn!(line, "device monitor: event dropped (channel full)");
                        }
                    }
                }
            }
        }
    }
}

// Parses a non-indented line from `ip monitor link address route`.
//
// Line formats (from iproute2 text output):
//   link:    "<index>: <name>: <flags>..."        — second ": " required, name has no spaces
//   address: "<index>: <name><whitespace>..."     — whitespace (not colon) follows name
//   route:   everything else (IP prefix, keywords like "blackhole", etc.)
//
// Deleted events are prefixed with "Deleted ".
fn parse_event(line: &str) -> NetworkEvent {
    let (deleted, rest) = match line.strip_prefix("Deleted ") {
        Some(r) => (true, r),
        None => (false, line),
    };

    // Both link and address lines start with "<index>: <name>...".
    if let Some((idx_str, after_idx)) = rest.split_once(": ")
        && let Ok(index) = idx_str.parse::<u32>()
    {
        // Link: name is followed by ": " (flags start)
        if let Some((name, _)) = after_idx.split_once(": ")
            && !name.contains(char::is_whitespace)
        {
            return if deleted {
                NetworkEvent::LinkRemoved {
                    index,
                    name: name.to_owned(),
                }
            } else {
                NetworkEvent::LinkChanged {
                    index,
                    name: name.to_owned(),
                }
            };
        }

        // Address: name is followed by whitespace (e.g. "   inet 10.x.x.x/32")
        // No need to require "inet"/"inet6" — no other indexed lines use this format.
        if let Some((name, _)) = after_idx.split_once(char::is_whitespace)
            && !name.is_empty()
        {
            return if deleted {
                NetworkEvent::AddressRemoved {
                    index,
                    name: name.to_owned(),
                }
            } else {
                NetworkEvent::AddressAdded {
                    index,
                    name: name.to_owned(),
                }
            };
        }
    }

    NetworkEvent::RouteChanged
}
