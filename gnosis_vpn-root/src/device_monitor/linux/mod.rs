mod ip_text;
mod rtnetlink;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::NetworkEvent;

pub async fn start(
    tx: mpsc::Sender<NetworkEvent>,
) -> std::io::Result<(CancellationToken, tokio::task::JoinHandle<()>)> {
    if rtnetlink::probe_multicast().await {
        tracing::info!("device monitor: using rtnetlink");
        return rtnetlink::start(tx);
    }
    tracing::warn!("device monitor: rtnetlink unavailable, falling back to ip monitor");

    Ok(ip_text::start(tx))
}
