use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

pub enum NetworkEvent {
    LinkChanged { index: u32, name: String },
    LinkRemoved { index: u32, name: String },
    AddressAdded { index: u32, name: String },
    AddressRemoved { index: u32, name: String },
    RouteAdded,
    RouteRemoved,
    RouteChanged,
}

pub fn start() -> std::io::Result<(
    CancellationToken,
    tokio::task::JoinHandle<()>,
    mpsc::Receiver<NetworkEvent>,
)> {
    let (tx, rx) = mpsc::channel(32);

    #[cfg(target_os = "linux")]
    {
        let (cancel, handle) = linux::start(tx)?;
        Ok((cancel, handle, rx))
    }

    #[cfg(target_os = "macos")]
    {
        let (cancel, handle) = macos::start_pf_route(tx);
        return Ok((cancel, handle, rx));
    }
}
