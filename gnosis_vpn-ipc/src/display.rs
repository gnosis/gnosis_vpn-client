//! Human-readable forms of the protocol types, as `gnosis_vpn-ctl` prints them.
use humantime::format_duration;

use std::fmt::{self, Display};
use std::time::SystemTime;

use crate::protocol::*;
use crate::values::Address;

/// `3m 12s`: the two most significant units of the time since `timestamp`.
pub fn elapsed(timestamp: &SystemTime) -> String {
    match timestamp.elapsed() {
        Ok(elapsed) => {
            let full = format_duration(elapsed).to_string();
            let cut = full.match_indices(' ').nth(1).map(|(i, _)| i).unwrap_or(full.len());
            full[..cut].to_string()
        }
        Err(e) => format!("error displaying duration: {e}"),
    }
}

/// Longest label value rendered before it is elided.
pub const META_FIELD_MAX_CHARS: usize = 64;

/// Marks a value configuration set itself, where the rest of the entry comes from discovery.
const CONFIG_VALUE: &str = " (c)";

/// Metadata is operator-published and unverified: drop characters that could rewrite or spoof terminal output.
pub fn sanitize_for_display(text: &str) -> String {
    let is_unsafe = |c: char| {
        c.is_control()
            || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
    };
    let cleaned: String = text.chars().filter(|c| !is_unsafe(*c)).collect();
    if cleaned.chars().count() <= META_FIELD_MAX_CHARS {
        return cleaned;
    }
    let kept: String = cleaned.chars().take(META_FIELD_MAX_CHARS).collect();
    format!("{kept}…")
}

impl DestinationSource {
    /// The origin code rendered beside the title: c is configuration, d is discovery.
    pub fn code(&self) -> &'static str {
        match self {
            DestinationSource::Configured => "c",
            DestinationSource::Discovered => "d",
            DestinationSource::ConfiguredAndDiscovered => "c+d",
        }
    }
}

impl Destination {
    /// `->()->()->` for two relays, `->` for a direct route.
    pub fn pretty_print_path(&self) -> String {
        let hops = self.hops.count();
        if hops == 0 {
            return "->".to_string();
        }
        let path = (0..hops).map(|_| "()").collect::<Vec<&str>>().join("->");
        format!("->{path}->")
    }

    /// The recognized labels, plus any target configuration pinned.
    fn labels_str(&self) -> String {
        // On a c-only or d-only entry the origin code already says who set every value.
        let mixed_origins = self.source == DestinationSource::ConfiguredAndDiscovered;
        let pinned = |key: &str| mixed_origins && self.overrides.configured_meta.contains_key(key);

        let mut parts = Vec::new();
        if let Some(name) = &self.meta.name {
            let shown = sanitize_for_display(name);
            // A discovered id is this name slugged plus the key hash, so only a name spelled like it says nothing new.
            let key_hash = format!("-{}", exit_discriminator(&self.address, self.hops));
            let own_id = self.id.as_str().strip_suffix(&key_hash).unwrap_or(self.id.as_str());
            let adds_nothing = shown == own_id;
            if pinned("name") || !adds_nothing {
                let mark = if pinned("name") { CONFIG_VALUE } else { "" };
                parts.push(format!("name: {shown}{mark}"));
            }
        }
        if let Some(geo) = &self.geo {
            parts.push(format!("country: {}", geo.country));
        }
        if let Some(description) = &self.meta.description {
            let mark = if pinned("description") { CONFIG_VALUE } else { "" };
            parts.push(format!("description: {}{mark}", sanitize_for_display(description)));
        }
        let targets = [
            (
                "gnosis_vpn_server",
                self.gnosis_vpn_server,
                self.overrides.configured_gnosis_vpn_server,
            ),
            (
                "wireguard_server",
                self.wireguard_server,
                self.overrides.configured_wireguard_server,
            ),
        ];
        for (key, value, configured) in targets {
            if mixed_origins && configured.is_some() {
                parts.push(format!("{key}: {value}{CONFIG_VALUE}"));
            }
        }
        parts.join(", ")
    }
}

impl Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let labels = self.labels_str();
        let labels = if labels.is_empty() {
            String::new()
        } else {
            format!(", {labels}")
        };
        write!(
            f,
            "{id} [{source}] (Exit: {address}, Route: (entry){path}({short}){labels})",
            id = self.id,
            source = self.source.code(),
            address = self.address,
            path = self.pretty_print_path(),
            short = self.address.short(),
        )
    }
}

impl Display for RunMode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RunMode::Init { last_error: None } => write!(f, "Initializing"),
            RunMode::Init { last_error: Some(err) } => write!(f, "Initializing (last error: {err})"),
            RunMode::PreparingSafe {
                node_address,
                node_xdai,
                node_wxhopr,
                funding_tool,
                error,
                balance_recommendation,
            } => {
                let wxhopr_sci = scientific_suffix(node_wxhopr);
                write!(
                    f,
                    "Preparing Safe (node: {node_address}, xdai: {node_xdai}, wxHOPR: {node_wxhopr}{wxhopr_sci}"
                )?;
                if let Some(recommendation) = balance_recommendation {
                    let required_sci = scientific_suffix(&recommendation.wxhopr);
                    write!(
                        f,
                        ", required: {}{required_sci} & {}",
                        recommendation.wxhopr, recommendation.xdai
                    )?;
                }
                if let Some(tool) = funding_tool {
                    write!(f, ", funding tool: {tool}")?;
                }
                if let Some(error) = error {
                    write!(f, ", error: {error}")?;
                }
                write!(f, ")")
            }
            RunMode::DeployingSafe { node_address } => write!(f, "Deploying Safe (node: {node_address})"),
            RunMode::Warmup { stage, last_error } => match last_error {
                None => write!(f, "Warmup ({stage})"),
                Some(err) => write!(f, "Warmup ({stage}, last error: {err})"),
            },
            RunMode::Running {
                hopr_status,
                funding_status,
                channel_maintenance,
            } => {
                match hopr_status {
                    Some(s) => write!(f, "Ready ({s})")?,
                    None => write!(f, "Ready")?,
                }
                match funding_status {
                    None => write!(f, " - waiting for funding calculations")?,
                    Some(status) => write!(f, " - traffic: {}, gas: {}", status.traffic, status.gas)?,
                }
                if let ChannelMaintenance::Unavailable { since } = channel_maintenance {
                    write!(f, ", channel maintenance: unavailable {}", elapsed(since))?;
                }
                Ok(())
            }
            RunMode::Shutdown => write!(f, "Shutting down"),
            RunMode::Restarting => write!(f, "Worker restarting"),
            RunMode::NotRunning => write!(f, "Worker offline"),
        }
    }
}

/// ` (7.5e-10)` for amounts too small to read as decimals, empty otherwise.
pub fn scientific_suffix<C>(balance: &crate::values::Balance<C>) -> String {
    balance.scientific().map(|s| format!(" ({s})")).unwrap_or_default()
}

impl Display for WarmupStage {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let text = match self {
            WarmupStage::Initializing => "Initializing",
            WarmupStage::ValidatingConfig => "Validating configuration",
            WarmupStage::IdentifyingNode => "Identifying node",
            WarmupStage::ConnectingBlockchain => "Connecting blockchain",
            WarmupStage::CreatingNode => "Creating node",
            WarmupStage::StartingNode => "Starting node",
            WarmupStage::Ready => "Ready",
            WarmupStage::Uninitialized => "Node is not yet initialized",
            WarmupStage::WaitingForFunds => "Waiting for initial wallet funding",
            WarmupStage::CheckingBalance => "Verifying wallet balance",
            WarmupStage::ValidatingNetworkConfig => "Validating network configuration",
            WarmupStage::CheckingOnchainAddress => "Checking onchain address",
            WarmupStage::RegisteringSafe => "Registering Safe contract",
            WarmupStage::AnnouncingNode => "Announcing node on chain",
            WarmupStage::AwaitingKeyBinding => "Waiting for on-chain key binding confirmation",
            WarmupStage::InitializingServices => "Initializing internal services",
            WarmupStage::Running => "Node is running",
            WarmupStage::Terminated => "Node has been terminated",
            WarmupStage::Degraded => "Node is running in degraded state",
            WarmupStage::Failed => "Node has failed",
        };
        f.write_str(text)
    }
}

impl Display for HoprStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", WarmupStage::from(*self))
    }
}

impl Display for Connection {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Connection::Disconnected => write!(f, "Disconnected"),
            Connection::Connecting {
                destination,
                since,
                phase,
            } => write!(
                f,
                "Connecting to {destination} (since {}, phase {phase})",
                elapsed(since)
            ),
            Connection::Reconnecting {
                destination,
                since,
                phase,
            } => {
                let progress = match phase {
                    Some(phase) => format!("phase {phase}"),
                    None => "waiting for route health".to_string(),
                };
                write!(
                    f,
                    "Reconnecting to {destination} (since {}, {progress})",
                    elapsed(since)
                )
            }
            Connection::Connected {
                destination,
                since,
                tunnel_ping_rtt,
                stall,
            } => {
                write!(f, "Connected to {destination} (since {})", elapsed(since))?;
                if let Some(rtt) = tunnel_ping_rtt {
                    write!(f, ", tunnel ping RTT {:.2} s", rtt.as_secs_f32())?;
                }
                if let Some(stall) = stall {
                    write!(
                        f,
                        ", stalled {} ({}/{})",
                        elapsed(&stall.since),
                        stall.failed_pings,
                        stall.reconnect_at
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl Display for Disconnecting {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Disconnecting from {} (since {}, phase {})",
            self.destination,
            elapsed(&self.since),
            self.phase
        )
    }
}

impl Display for ConnectPhase {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let text = match self {
            ConnectPhase::Init => "Init",
            ConnectPhase::GatherPeerIps => "Retrieving peer IPs",
            ConnectPhase::KillswitchLockdown => "Activating killswitch",
            ConnectPhase::GeneratingWg => "Generating WireGuard keypairs",
            ConnectPhase::RegisterWg => "Registering WireGuard public key",
            ConnectPhase::OpeningPing => "Opening main connection",
            ConnectPhase::EstablishWgTunnel => "Establishing WireGuard tunnel",
            ConnectPhase::VerifyPing => "Verifying established connection",
            ConnectPhase::AdjustToMain => "Upgrading for general traffic",
            ConnectPhase::ConnectionEstablished => "Connection established",
        };
        f.write_str(text)
    }
}

impl Display for DisconnectPhase {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let text = match self {
            DisconnectPhase::Disconnecting => "Disconnecting",
            DisconnectPhase::OpeningBridge => "Opening bridge connection",
            DisconnectPhase::UnregisterWg => "Unregistering WireGuard public key",
            DisconnectPhase::ClosingBridge => "Closing bridge connection",
        };
        f.write_str(text)
    }
}

impl Display for ProbeView {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Probing {} - {}", self.destination, self.state)?;
        if let Some(since) = &self.session_since {
            write!(f, " (session since {})", elapsed(since))?;
        }
        if let Some(rtt) = self.ping_rtt {
            write!(f, ", ping RTT {:.2} s", rtt.as_secs_f32())?;
        }
        if let Some(load) = &self.load {
            write!(f, ", {load}")?;
        }
        match (&self.versions, &self.api_version) {
            (Some(versions), Some(api)) => write!(f, ", API {api} ({versions})")?,
            (Some(versions), None) => write!(f, ", no compatible API ({versions})")?,
            (None, _) => {}
        }
        if let Some(checked_at) = &self.checked_at {
            write!(f, ", last check {} ago", elapsed(checked_at))?;
        }
        if self.consecutive_failures > 0 {
            write!(f, " ({} consecutive failures)", self.consecutive_failures)?;
        }
        if let Some(err) = &self.last_error {
            write!(f, " (last error: {err})")?;
        }
        Ok(())
    }
}

impl Display for ProbeState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ProbeState::Opening => write!(f, "opening session"),
            ProbeState::Checking => write!(f, "running initial checks"),
            ProbeState::Ready => write!(f, "ready"),
            ProbeState::Reopening => write!(f, "reopening session"),
        }
    }
}

impl Display for ExitVersions {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let api = self.api.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ");
        match &self.server {
            Some(server) => write!(f, "server {server}, supported: {api}"),
            None => write!(f, "supported: {api}"),
        }
    }
}

impl Display for Health {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}, {}", self.load_avg, self.slots)
    }
}

impl Display for Slots {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Whatever is neither in use nor free is held by a registration still settling.
        let pending = self.total.saturating_sub(self.connected.saturating_add(self.available));
        write!(f, "slots: {}", self.connected)?;
        if pending > 0 {
            write!(f, "({pending} pending)")?;
        }
        write!(f, " / {}", self.total)
    }
}

impl Display for LoadAvg {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{:.2}/{n} (1 min), {:.2}/{n} (5 min), {:.2}/{n} (15 min)",
            self.one,
            self.five,
            self.fifteen,
            n = self.nproc
        )
    }
}

impl Display for RouteHealthView {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.state)?;
        if let Some(err) = &self.last_error {
            write!(f, " (last error: {err})")?;
        }
        Ok(())
    }
}

impl Display for RouteHealthState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RouteHealthState::Unrecoverable { reason } => write!(f, "Unrecoverable: {reason}"),
            RouteHealthState::NotRoutable => write!(f, "Not routable - no path in the network graph"),
            RouteHealthState::Routable => write!(f, "Routable"),
        }
    }
}

impl Display for UnrecoverableReason {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            UnrecoverableReason::NotAllowed => write!(
                f,
                "routing mode not allowed; use --allow-insecure for 0-hop or --allow-experimental for 2+ hops"
            ),
            UnrecoverableReason::IncompatibleApiVersion { offered } => {
                let offered = offered.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ");
                write!(
                    f,
                    "exit server offers no compatible API version (server offers: {offered})"
                )
            }
            UnrecoverableReason::CannotOpenSession { error } => {
                write!(f, "cannot open a session with this connection config: {error}")
            }
        }
    }
}

impl Display for RouteWalk {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RouteWalk::NotAnnounced { walked_at } => {
                write!(f, "exit not announced on chain - walked {} ago", elapsed(walked_at))
            }
            RouteWalk::NoPath {
                walked_at,
                forward_count,
                return_count,
            } => write!(
                f,
                "no path found (forward {forward_count}, return {return_count}) - walked {} ago",
                elapsed(walked_at)
            ),
            RouteWalk::Paths {
                walked_at,
                count,
                distinct_first_relays,
                best_relays,
                best_value,
                return_count,
                return_best_relays,
                return_best_value,
            } => {
                let plural = if *count == 1 { "" } else { "s" };
                write!(f, "{count} path{plural}, ")?;
                // Only a 0-hop walk has no first relay; empty relays alone may just be unresolved.
                let is_direct = *distinct_first_relays == 0;
                if is_direct {
                    write!(f, "direct")?;
                } else {
                    write!(
                        f,
                        "{distinct_first_relays} distinct first relays, {}",
                        best_via(best_relays)
                    )?;
                }
                write!(f, " (value {best_value:.3}); return {return_count}, ")?;
                if is_direct {
                    write!(f, "direct")?;
                } else {
                    write!(f, "{}", best_via(return_best_relays))?;
                }
                write!(f, " (value {return_best_value:.3}) - walked {} ago", elapsed(walked_at))
            }
        }
    }
}

fn best_via(relays: &[Address]) -> String {
    if relays.is_empty() {
        return "unresolved relays".to_string();
    }
    let chain = relays.iter().map(Address::short).collect::<Vec<_>>().join(" -> ");
    format!("best via {chain}")
}

impl Display for QuickProbeCheck {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "checked {} ago - status RTT {:.2} s, {}",
            elapsed(&self.checked_at),
            self.status_rtt.as_secs_f32(),
            self.load
        )?;
        match &self.api_version {
            Some(api) => write!(f, ", API {api} ({})", self.versions),
            None => write!(f, ", no compatible API ({})", self.versions),
        }
    }
}

impl Display for QuickProbeState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            QuickProbeState::Checking { since, last } => {
                write!(f, "checking (since {})", elapsed(since))?;
                match last {
                    Some(check) => write!(f, " - last {check}"),
                    None => Ok(()),
                }
            }
            QuickProbeState::Checked(check) => write!(f, "{check}"),
            QuickProbeState::Failed { checked_at, error } => {
                write!(f, "failed {} ago: {error}", elapsed(checked_at))
            }
        }
    }
}

impl Display for QuickProbeResponse {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            QuickProbeResponse::Checking { destination } => {
                write!(f, "Checking {destination} - its result will appear in status")
            }
            QuickProbeResponse::UnableToProbe {
                destination,
                route_health,
            } => write!(f, "Unable to check {destination}: {route_health}"),
            QuickProbeResponse::AlreadyProbing { destination } => {
                write!(f, "Already probing {destination} - its results are in status")
            }
            QuickProbeResponse::AlreadyChecking { destination } => {
                write!(f, "Already checking {destination} - its result will appear in status")
            }
            QuickProbeResponse::NotReady => write!(f, "Edge client not running yet - try again later"),
            QuickProbeResponse::DestinationNotFound => write!(f, "Destination not found"),
            QuickProbeResponse::DestinationAmbiguous { candidates } => write!(
                f,
                "That exit is reachable by several paths - check one of: {}",
                join(candidates)
            ),
        }
    }
}

/// Comma-separated, for id lists in one line.
pub fn join<T: Display>(items: &[T]) -> String {
    items.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ")
}

impl Display for FundingLevel {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let text = match self {
            FundingLevel::Good => "Good",
            FundingLevel::Low => "Low",
            FundingLevel::Empty => "Empty",
        };
        f.write_str(text)
    }
}

impl Display for ChannelOut {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.matched_exit {
            Some(id) => write!(f, "Channel({},{id}): ", self.address)?,
            None => write!(f, "Channel({}): ", self.address)?,
        }
        match &self.balance {
            None => write!(f, "unknown balance"),
            Some(amount) => write!(f, "{amount}{}", scientific_suffix(amount)),
        }
    }
}

impl Display for NodeInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Info(node_address: {}, safe_address: {})",
            self.node_address, self.safe_address
        )
    }
}
