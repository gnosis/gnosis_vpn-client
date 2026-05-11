use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        mod linux;
        use linux::Actor;
        pub use linux::Msg;
    } else {
        mod stub;
        use stub::Actor;
        pub use stub::Msg;
    }
}

pub fn start(cancel: CancellationToken) -> mpsc::Sender<Msg> {
    let (sender, receiver) = mpsc::channel(32);
    tokio::spawn(run(receiver, cancel));
    sender
}

async fn run(mut receiver: mpsc::Receiver<Msg>, cancel: CancellationToken) {
    tracing::info!("routing actor started");
    let mut actor = Actor::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("routing actor stopping");
                break;
            }
            msg = receiver.recv() => match msg {
                Some(msg) => actor.handle(msg),
                None => {
                    tracing::info!("routing actor channel closed");
                    break;
                }
            }
        }
    }
    actor.teardown();
}
