use gnosis_vpn_lib::{event, worker};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[derive(Debug)]
pub struct Dynamic {
    worker: worker::Worker,
    wg_data: event::WireGuardData,
}

impl Dynamic {
    pub fn new(worker: worker::Worker, wg_data: event::WireGuardData) -> Self {
        Self { worker, wg_data }
    }
}
