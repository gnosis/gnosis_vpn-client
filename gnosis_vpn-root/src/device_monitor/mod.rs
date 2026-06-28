use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

pub enum NetworkEvent {
    LinkChanged {
        index: u32,
        name: String,
    },
    LinkRemoved {
        index: u32,
        name: String,
    },
    AddressAdded {
        index: u32,
        name: String,
    },
    AddressRemoved {
        index: u32,
        name: String,
    },
    RouteAdded,
    RouteRemoved,
    #[cfg(target_os = "macos")]
    RouteChanged,
}

pub fn start(tx: mpsc::Sender<NetworkEvent>) -> std::io::Result<(CancellationToken, tokio::task::JoinHandle<()>)> {
    #[cfg(target_os = "linux")]
    {
        linux::start(tx)
    }

    #[cfg(target_os = "macos")]
    {
        Ok(macos::start_pf_route(tx))
    }
}
