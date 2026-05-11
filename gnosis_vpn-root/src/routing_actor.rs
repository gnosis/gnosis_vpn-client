use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Placeholder — variants will be added when the messaging protocol is defined.
pub enum Msg {}

pub fn start(cancel: CancellationToken) -> mpsc::Sender<Msg> {
    let (sender, receiver) = mpsc::channel(32);
    tokio::spawn(run(receiver, cancel));
    sender
}

async fn run(mut receiver: mpsc::Receiver<Msg>, cancel: CancellationToken) {
    tracing::info!("routing actor started");
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("routing actor stopping");
                break;
            }
            msg = receiver.recv() => match msg {
                Some(msg) => match msg {},
                None => {
                    tracing::info!("routing actor channel closed");
                    break;
                }
            }
        }
    }
}
