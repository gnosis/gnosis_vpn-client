use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::device_monitor::NetworkEvent;

/// Probes whether `ip -j` JSON output is supported on this system.
///
/// Runs `ip -j link show lo` — a fast, deterministic command — and checks
/// that it exits successfully and produces parseable JSON. This catches both
/// old iproute2 that doesn't recognise -j (exits non-zero) and hypothetical
/// versions that silently ignore it and output plain text.
pub async fn probe() -> bool {
    let out = tokio::process::Command::new("ip")
        .args(["-j", "link", "show", "lo"])
        .output()
        .await;

    match out {
        Err(e) => {
            tracing::debug!(error = ?e, "device monitor probe: failed to run ip -j");
            false
        }
        Ok(out) if !out.status.success() => {
            tracing::debug!("device monitor probe: ip -j exited with error");
            false
        }
        Ok(out) => serde_json::from_slice::<serde_json::Value>(&out.stdout).is_ok(),
    }
}

pub fn start(tx: mpsc::Sender<NetworkEvent>) -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(run(tx, cancel.clone()));
    (cancel, handle)
}

async fn run(tx: mpsc::Sender<NetworkEvent>, cancel: CancellationToken) {
    use tokio::io::AsyncBufReadExt;

    tracing::debug!("device monitor started (ip -j monitor)");
    let child = tokio::process::Command::new("ip")
        .args(["-j", "monitor", "link", "address", "route"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = ?e, "device monitor: failed to spawn ip -j monitor");
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
                    tracing::error!(error = ?e, "device monitor: ip -j monitor read error");
                    return;
                }
                Ok(None) => {
                    tracing::warn!("device monitor: ip -j monitor exited unexpectedly");
                    return;
                }
                Ok(Some(line)) if !line.is_empty() => {
                    match parse_event(&line) {
                        Some(event) => {
                            if tx.try_send(event).is_err() {
                                tracing::warn!(line, "device monitor: event dropped (channel full)");
                            }
                        }
                        None => tracing::debug!(line, "device monitor: unrecognised ip -j monitor line"),
                    }
                }
                Ok(Some(_)) => {}
            }
        }
    }
}

// Parses one NDJSON event line from `ip -j monitor`.
//
// There is no type-discriminator field. We detect event type by which fields
// are present (verified against iproute2 6.x source):
//   link:    "ifname" present  → RTM_NEWLINK / RTM_DELLINK
//   address: "dev" + "local"   → RTM_NEWADDR / RTM_DELADDR
//   route:   everything else   → RTM_NEWROUTE / RTM_DELROUTE
// Deletions carry "deleted": true.
fn parse_event(line: &str) -> Option<NetworkEvent> {
    let val: serde_json::Value = serde_json::from_str(line).ok()?;
    let deleted = val["deleted"].as_bool().unwrap_or(false);

    // Link event
    if let Some(name) = val["ifname"].as_str() {
        let index = val["ifindex"].as_u64().unwrap_or(0) as u32;
        let name = name.to_owned();
        return Some(if deleted {
            NetworkEvent::LinkRemoved { index, name }
        } else {
            NetworkEvent::LinkChanged { index, name }
        });
    }

    // Address event: "dev" carries the interface name, "local" the assigned IP
    if let (Some(name), true) = (val["dev"].as_str(), val.get("local").is_some()) {
        let index = val["index"].as_u64().unwrap_or(0) as u32;
        let name = name.to_owned();
        return Some(if deleted {
            NetworkEvent::AddressRemoved { index, name }
        } else {
            NetworkEvent::AddressAdded { index, name }
        });
    }

    // Route event
    Some(if deleted {
        NetworkEvent::RouteRemoved
    } else {
        NetworkEvent::RouteAdded
    })
}
