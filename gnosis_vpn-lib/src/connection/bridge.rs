//! Bridge sessions to the exit's gnosis-vpn server.
//!
//! Both runners talk HTTP to the exit's server through a HOPR session bound to a loopback port:
//! the connection runner registers the WireGuard key, the disconnection runner unregisters it.
//! A caller that opens a session here owns it and must [`close`] it.

use edgli::FlowControlConfig;
use edgli::hopr_lib::HoprSessionClientConfig;

use crate::connection::destination::Destination;
use crate::connection::options::{Options, SurbParams};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};

/// Open a single-client bridge session to the exit's gnosis-vpn server.
pub(crate) async fn open(
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
pub(crate) async fn close(hopr: &Hopr, session_client_metadata: &SessionClientMetadata) -> Result<(), HoprError> {
    let res = hopr
        .close_session(session_client_metadata.bound_host, session_client_metadata.protocol)
        .await;
    match res {
        Ok(_) => Ok(()),
        Err(HoprError::SessionNotFound) => {
            tracing::warn!(bound_host = ?session_client_metadata.bound_host, "attempted to close bridge session but it was not found, possibly already closed");
            Ok(())
        }
        Err(e) => Err(e),
    }
}
