mod rtnetlink;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::NetworkEvent;

pub fn start(tx: mpsc::Sender<NetworkEvent>) -> std::io::Result<(CancellationToken, tokio::task::JoinHandle<()>)> {
    rtnetlink::start(tx)
}
