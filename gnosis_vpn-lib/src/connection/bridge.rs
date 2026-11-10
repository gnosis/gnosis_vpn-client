//! Bridge sessions to the exit's gnosis-vpn server.
//!
//! Opened for an unregister when there is no probe session; [`Owned`] closes it even if the runner is cancelled.

use std::sync::Arc;

use edgli::FlowControlConfig;
use edgli::hopr_lib::HoprSessionClientConfig;

use crate::connection::destination::Destination;
use crate::connection::options::{Options, SurbParams};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};

/// Open a single-client bridge session to the exit's gnosis-vpn server.
async fn open(
    hopr: &Hopr,
    destination: &Destination,
    options: &Options,
    surb: SurbParams,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = HoprSessionClientConfig {
        capabilities: options.sessions.bridge.capabilities,
        forward_path: destination.routing,
        return_path: destination.routing,
        max_surbs_per_data_packet: surb.max_surbs_per_data_packet,
        surb_management: surb.management,
        // Robust tail-tolerance profile for the bridge's short HTTP exchange.
        flow_control: Some(FlowControlConfig::robust()),
        ..Default::default()
    };
    let cfg = if options.pix.bridge.enabled {
        hopr.pix_aware_session_cfg(cfg)?
    } else {
        cfg
    };
    hopr.open_session(
        destination.address,
        destination.bridge_target(),
        Some(1),
        Some(1),
        cfg.clone(),
    )
    .await
}

/// Close a bridge session opened by [`open`]; already-gone is the same as closed.
async fn close(hopr: &Hopr, session_client_metadata: &SessionClientMetadata) -> Result<(), HoprError> {
    let res = hopr
        .close_session(session_client_metadata.bound_host, session_client_metadata.protocol)
        .await;
    match res {
        Ok(_) => Ok(()),
        Err(HoprError::SessionNotFound) => {
            tracing::debug!(bound_host = ?session_client_metadata.bound_host, "bridge session already closed");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// A bridge session this module opened, closed on drop.
///
/// A runner can be cancelled at any await between opening and closing (disconnect, target switch, a
/// WAN-change force reconnect). Only `close_session` and a hopr shutdown drop the listener entry, so
/// an unclosed session would linger with its listener for the rest of the worker's life.
pub(crate) struct Owned {
    hopr: Arc<Hopr>,
    session: SessionClientMetadata,
    armed: bool,
}

impl Owned {
    pub(crate) async fn open(
        hopr: Arc<Hopr>,
        destination: &Destination,
        options: &Options,
        surb: SurbParams,
    ) -> Result<Self, HoprError> {
        let session = open(&hopr, destination, options, surb).await?;
        Ok(Self {
            hopr,
            session,
            armed: true,
        })
    }

    pub(crate) fn session(&self) -> &SessionClientMetadata {
        &self.session
    }

    /// Close now instead of at drop; a failure here is reported but the session is still considered gone.
    pub(crate) async fn close(&mut self) -> Result<(), HoprError> {
        let res = close(&self.hopr, &self.session).await;
        // Disarm only once the close ran: a cancel during the await must still fall back to `Drop`.
        self.armed = false;
        res
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Drop cannot await; hopr's close is quick and the task outlives the runner.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(bound_host = ?self.session.bound_host, "no runtime to close an owned bridge session on drop");
            return;
        };
        let hopr = self.hopr.clone();
        let session = self.session.clone();
        runtime.spawn(async move {
            if let Err(error) = close(&hopr, &session).await {
                tracing::warn!(%error, bound_host = ?session.bound_host, "failed to close an owned bridge session");
            }
        });
    }
}
