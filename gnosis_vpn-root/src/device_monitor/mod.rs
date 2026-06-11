use tokio_util::sync::CancellationToken;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

pub async fn start() -> std::io::Result<(CancellationToken, tokio::task::JoinHandle<()>)> {
    #[cfg(target_os = "linux")]
    {
        if linux::probe_rtnetlink_multicast().await {
            tracing::info!("device monitor: using rtnetlink");
            return linux::start_rtnetlink();
        }
        tracing::warn!("device monitor: rtnetlink multicast not working, falling back to ip monitor subprocess");
        Ok(linux::start_subprocess())
    }

    #[cfg(target_os = "macos")]
    return Ok(macos::start_pf_route());
}
