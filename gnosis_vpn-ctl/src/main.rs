use exitcode::{self, ExitCode};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};

use std::fmt;
use std::io::{self, IsTerminal, Write as _};
use std::path::Path;
use std::process;

use gnosis_vpn_lib::balance;
use gnosis_vpn_lib::check_update::Channel;
use gnosis_vpn_lib::command::{self, Command, Response};
use gnosis_vpn_lib::socket;
use gnosis_vpn_lib::update::{UpdateStage, UpdateStatus};

mod cli;

use cli::OutputFormat;

// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() {
    let args = cli::parse();
    let format = args.output.unwrap_or(OutputFormat::Plain);

    if let cli::Command::Completions { shell } = args.command {
        cli::generate_completions(shell);
        process::exit(exitcode::OK);
    }

    if let cli::Command::CheckUpdate { force, channel } = args.command {
        let exit = run_check_update(format, &args.socket_path, channel.into(), force).await;
        process::exit(exit);
    }
    if let cli::Command::InstallUpdate {
        channel,
        yes,
        allow_downgrade,
        force,
    } = args.command
    {
        let exit = run_install_update(format, &args.socket_path, channel.into(), yes, allow_downgrade, force).await;
        process::exit(exit);
    }

    let cmd: Command = args.command.into();
    let resp = match socket::root::process_cmd(&args.socket_path, &cmd).await {
        Ok(resp) => resp,
        Err(e) => {
            eprintln!("Error processing {cmd}: {e}");
            process::exit(exitcode::UNAVAILABLE);
        }
    };

    match format {
        OutputFormat::Json => json_print(&resp),
        OutputFormat::Yaml => yaml_print(&resp),
        OutputFormat::Plain => pretty_print(&resp),
    };

    let exit = determine_exitcode(&resp);
    process::exit(exit);
}

async fn run_check_update(format: OutputFormat, socket_path: &Path, channel: Channel, force: bool) -> ExitCode {
    let resp = match socket::root::process_cmd(socket_path, &Command::CheckUpdate { channel, force }).await {
        Ok(r) => r,
        Err(e) => return emit_check_update_error(format, CheckUpdateErrorKind::Unavailable, &e.to_string()),
    };
    let Response::CheckUpdate(result) = resp else {
        return emit_check_update_error(
            format,
            CheckUpdateErrorKind::Internal,
            &format!("unexpected response: {resp:?}"),
        );
    };
    match format {
        OutputFormat::Json => match serde_json::to_string_pretty(&result) {
            Ok(s) => println!("{s}"),
            Err(e) => return emit_check_update_error(format, CheckUpdateErrorKind::Internal, &e.to_string()),
        },
        OutputFormat::Yaml => match serde_saphyr::to_string(&result) {
            Ok(s) => print!("{s}"),
            Err(e) => return emit_check_update_error(format, CheckUpdateErrorKind::Internal, &e.to_string()),
        },
        OutputFormat::Plain => match &result {
            command::CheckUpdateResponse::UpToDate { current } => {
                println!("Up to date (current {current}).");
            }
            command::CheckUpdateResponse::Available { current, release } => {
                println!(
                    "Update available on {channel}: {} (current {current}, published {})\n  download: {}",
                    release.version, release.published_at, release.download_url
                );
            }
            command::CheckUpdateResponse::NoReleaseForChannel(ch) => {
                eprintln!("No release on channel {ch}");
            }
            command::CheckUpdateResponse::VpnNotConnected => {
                return emit_check_update_error(
                    OutputFormat::Plain,
                    CheckUpdateErrorKind::VpnNotConnected,
                    "checking for updates without an active VPN connection is insecure",
                );
            }
            command::CheckUpdateResponse::IntegrityError(msg) => {
                return emit_check_update_error(OutputFormat::Plain, CheckUpdateErrorKind::IntegrityError, msg);
            }
            command::CheckUpdateResponse::Error(msg) => {
                return emit_check_update_error(OutputFormat::Plain, CheckUpdateErrorKind::Internal, msg);
            }
        },
    }
    match result {
        command::CheckUpdateResponse::UpToDate { .. } | command::CheckUpdateResponse::Available { .. } => exitcode::OK,
        command::CheckUpdateResponse::NoReleaseForChannel(_) => exitcode::UNAVAILABLE,
        command::CheckUpdateResponse::VpnNotConnected => CheckUpdateErrorKind::VpnNotConnected.exit_code(),
        command::CheckUpdateResponse::IntegrityError(_) => CheckUpdateErrorKind::IntegrityError.exit_code(),
        command::CheckUpdateResponse::Error(_) => exitcode::SOFTWARE,
    }
}

async fn run_install_update(
    format: OutputFormat,
    socket_path: &Path,
    channel: Channel,
    yes: bool,
    allow_downgrade: bool,
    force: bool,
) -> ExitCode {
    if !yes && matches!(format, OutputFormat::Plain) && io::stdin().is_terminal() {
        eprint!(
            "Install update on channel {channel}{}? [y/N] ",
            if allow_downgrade { " (with downgrade)" } else { "" }
        );
        let _ = io::stderr().flush();
        let mut buf = String::new();
        if io::stdin().read_line(&mut buf).is_err() {
            return exitcode::IOERR;
        }
        if !matches!(buf.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            eprintln!("Aborted.");
            return exitcode::OK;
        }
    }

    let mut stream = match gnosis_vpn_lib::update::install_stream(socket_path, channel, allow_downgrade, force).await {
        Ok(s) => Box::pin(s),
        Err(e) => return emit_check_update_error(format, CheckUpdateErrorKind::Unavailable, &e.to_string()),
    };

    let render = matches!(format, OutputFormat::Plain) && io::stderr().is_terminal();
    let mut bar: Option<ProgressBar> = None;
    let mut last_status: Option<UpdateStatus> = None;

    while let Some(item) = stream.next().await {
        let status = match item {
            Ok(s) => s,
            Err(e) => {
                if let Some(b) = bar.take() {
                    b.abandon();
                }
                return emit_check_update_error(format, CheckUpdateErrorKind::Internal, &e.to_string());
            }
        };
        match format {
            OutputFormat::Json => {
                if let Ok(line) = serde_json::to_string(&status) {
                    println!("{line}");
                }
            }
            OutputFormat::Yaml => {
                if let Ok(doc) = serde_saphyr::to_string(&status) {
                    print!("---\n{doc}");
                }
            }
            OutputFormat::Plain => {
                if render {
                    render_status(&status, &mut bar);
                } else {
                    eprintln!("{status}");
                }
            }
        }
        last_status = Some(status);
    }

    if let Some(b) = bar.take() {
        b.finish_and_clear();
    }

    match last_status {
        Some(UpdateStatus::Completed { new_version }) => {
            if matches!(format, OutputFormat::Plain) {
                eprintln!("Update complete: {new_version}. Service restart will be handled by launchd/systemd.");
            }
            exitcode::OK
        }
        Some(UpdateStatus::Failed { stage, error }) => {
            let kind = match stage {
                UpdateStage::Check => CheckUpdateErrorKind::Unavailable,
                UpdateStage::Download => CheckUpdateErrorKind::Unavailable,
                UpdateStage::Verify => CheckUpdateErrorKind::IntegrityError,
                UpdateStage::Install => CheckUpdateErrorKind::Internal,
            };
            emit_check_update_error(format, kind, &error)
        }
        _ => exitcode::IOERR,
    }
}

fn render_status(status: &UpdateStatus, bar: &mut Option<ProgressBar>) {
    match status {
        UpdateStatus::Downloading {
            bytes_done,
            bytes_total,
        } => {
            let pb = bar.get_or_insert_with(|| {
                let pb = ProgressBar::new(*bytes_total);
                pb.set_style(
                    ProgressStyle::with_template(
                        "{spinner} downloading [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
                    )
                    .expect("progress style"),
                );
                pb
            });
            pb.set_length(*bytes_total);
            pb.set_position(*bytes_done);
        }
        other => {
            if let Some(b) = bar.take() {
                b.finish_and_clear();
            }
            eprintln!("{other}");
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum CheckUpdateErrorKind {
    Unavailable,
    IntegrityError,
    Internal,
    VpnNotConnected,
}

impl CheckUpdateErrorKind {
    fn slug(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::IntegrityError => "integrity_error",
            Self::Internal => "internal",
            Self::VpnNotConnected => "vpn_not_connected",
        }
    }

    fn exit_code(self) -> ExitCode {
        match self {
            Self::Unavailable => exitcode::UNAVAILABLE,
            Self::VpnNotConnected => exitcode::NOPERM,
            Self::IntegrityError | Self::Internal => exitcode::SOFTWARE,
        }
    }
}

impl fmt::Display for CheckUpdateErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Unavailable => "Update manifest unavailable",
            Self::IntegrityError => "Update manifest integrity check failed",
            Self::Internal => "Internal error",
            Self::VpnNotConnected => "VPN not connected",
        };
        f.write_str(label)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CheckUpdateErrorPayload {
    r#type: String,
    error: String,
}

fn emit_check_update_error(format: OutputFormat, kind: CheckUpdateErrorKind, message: &str) -> ExitCode {
    let payload = CheckUpdateErrorPayload {
        r#type: kind.slug().to_string(),
        error: message.to_string(),
    };
    match format {
        OutputFormat::Json => {
            eprintln!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
        }
        OutputFormat::Yaml => {
            eprintln!("{}", serde_saphyr::to_string(&payload).unwrap_or_default());
        }
        OutputFormat::Plain => {
            eprintln!("{kind}: {message}");
        }
    }
    kind.exit_code()
}

fn json_print(resp: &Response) {
    match serde_json::to_string_pretty(resp) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("Error serializing response to JSON: {e}"),
    }
}

fn yaml_print(resp: &Response) {
    match serde_saphyr::to_string(resp) {
        Ok(s) => print!("{s}"),
        Err(e) => eprintln!("Error serializing response to YAML: {e}"),
    }
}

fn pretty_print(resp: &Response) {
    match resp {
        Response::Connect(command::ConnectResponse::AlreadyConnected(dest)) => {
            println!("Already connected to {dest}");
        }
        Response::Connect(command::ConnectResponse::Connecting(dest)) => {
            println!("Connecting to {dest}");
        }
        Response::Connect(command::ConnectResponse::WaitingToConnect(dest, route_health)) => {
            println!("Waiting to connect to {dest} once possible: {route_health}")
        }
        Response::Connect(command::ConnectResponse::UnableToConnect(dest, route_health)) => {
            eprintln!("Unable to connect to {dest}: {route_health}");
        }
        Response::Connect(command::ConnectResponse::DestinationNotFound) => {
            eprintln!("Destination not found");
        }
        Response::Disconnect(command::DisconnectResponse::Disconnecting(dest)) => {
            println!("Disconnecting from {dest}");
        }
        Response::Disconnect(command::DisconnectResponse::NotConnected) => {
            eprintln!("Currently not connected to any destination");
        }
        Response::Telemetry(Some(metrics)) => {
            println!("{metrics}");
        }
        Response::Telemetry(None) => {
            println!("No telemetry information available.");
        }
        Response::Status(command::StatusResponse {
            run_mode,
            destinations,
            target_destination,
            connecting,
            reconnecting,
            connected,
            disconnecting,
        }) => {
            let mut str_resp = format!("{run_mode}\n");
            if let Some(id) = target_destination {
                let is_active = connecting.as_ref().is_some_and(|c| c.destination_id == *id)
                    || reconnecting.as_ref().is_some_and(|c| c.destination_id == *id)
                    || connected.as_ref().is_some_and(|c| c.destination_id == *id);
                if !is_active {
                    str_resp.push_str(&format!("---\nWaiting to connect to {id}\n"));
                }
            }
            if let Some(info) = connecting {
                str_resp.push_str(&format!("---\n{info}\n"));
            }
            if let Some(info) = reconnecting {
                str_resp.push_str(&format!("---\n{info}\n"));
            }
            if let Some(info) = connected {
                str_resp.push_str(&format!("---\n{info}\n"));
            }
            for info in disconnecting {
                str_resp.push_str(&format!("---\n{info}\n"));
            }
            for dest_state in destinations {
                str_resp.push_str(&format!("---\n{}\n", dest_state.destination));
                if let Some(rh) = &dest_state.route_health {
                    str_resp.push_str(&format!("{} Route health: {}\n", dest_state.destination.id, rh,));
                }
            }
            println!("{str_resp}");
        }
        Response::Balance(Ok(command::BalanceResponse {
            node,
            safe,
            channels_out,
            info,
            capacity_allocations,
            ideal_balance: _,
            funding_issues,
        })) => {
            let mut str_resp = String::new();
            str_resp.push_str(&format!(
                "Node Address: {}\nSafe Address: {}\n",
                info.node_address.to_checksum(),
                info.safe_address.to_checksum()
            ));
            let safe_sci = balance::wxhopr_scientific(*safe)
                .map(|s| format!(" ({s})"))
                .unwrap_or_default();
            str_resp.push_str(&format!("---\nNode Balance: {node}\nSafe Balance: {safe}{safe_sci}\n"));
            if channels_out.is_empty() {
                str_resp.push_str("---\nNo outgoing channels.\n");
            } else {
                let allocations = capacity_allocations.as_deref().unwrap_or(&[]);
                let sci = balance::wxhopr_scientific(*safe)
                    .map(|s| format!(" ({s})"))
                    .unwrap_or_default();
                let safe_cap = find_capacity(allocations, &balance::CapacityAllocator::Safe);
                str_resp.push_str(&format!("Safe: {safe}{sci}{}\n", format_capacity(safe_cap)));
                for ch in channels_out {
                    let ch_cap = find_capacity(allocations, &balance::CapacityAllocator::Peer(ch.address));
                    str_resp.push_str(&format!("{ch}{}\n", format_capacity(ch_cap)));
                }
            }
            match funding_issues.as_deref() {
                None => str_resp.push_str("---\nWaiting for funding calculations\n"),
                Some([]) => str_resp.push_str("---\nWell funded\n"),
                Some(issues) => {
                    str_resp.push_str("---\n");
                    for issue in issues {
                        str_resp.push_str(&format!("Funding issue: {issue}\n"));
                    }
                }
            }
            println!("{str_resp}");
        }
        Response::Balance(Err(msg)) => {
            eprintln!("Balance error: {msg}");
        }
        Response::Pong => {
            println!("Pong");
        }
        Response::NerdStats(nerd_stats) => {
            print_nerd_stats(nerd_stats);
        }
        Response::FundingTool(command::FundingToolResponse::WrongPhase) => {
            eprintln!("Already past potential funding phase - no longer possible to fund");
        }
        Response::FundingTool(command::FundingToolResponse::Started) => {
            println!("Started funding");
        }
        Response::FundingTool(command::FundingToolResponse::InProgress) => {
            println!("Funding in progress");
        }
        Response::FundingTool(command::FundingToolResponse::Done) => {
            println!("Funding complete");
        }
        Response::Info(info) => {
            println!(
                "Gnosis VPN: client service version: {}, package version: {}{}",
                info.version,
                info.package_version.as_deref().unwrap_or("not available"),
                info.log_file
                    .as_ref()
                    .map(|f| format!("\nLog file: {}", f.display()))
                    .unwrap_or_default(),
            );
        }
        Response::StartClient(command::StartClientResponse::Started) => {
            println!("Worker client started");
        }
        Response::StartClient(command::StartClientResponse::AlreadyRunning) => {
            eprintln!("Worker client already running");
        }
        Response::StopClient(command::StopClientResponse::Stopped) => {
            println!("Worker client stopped");
        }
        Response::StopClient(command::StopClientResponse::NotRunning) => {
            eprintln!("Worker client not running");
        }
        Response::Destinations(ids) => {
            for id in ids {
                println!("{id}");
            }
        }
        Response::WorkerOffline => {
            eprintln!("Worker client is currently offline - use command `start-client` to start it");
        }
        Response::CheckUpdate(command::CheckUpdateResponse::UpToDate { current }) => {
            println!("Up to date (current {current}).");
        }
        Response::CheckUpdate(command::CheckUpdateResponse::Available { current, release }) => {
            println!(
                "Update available: {} (current {}, published {})\n  download: {}",
                release.version, current, release.published_at, release.download_url
            );
        }
        Response::CheckUpdate(command::CheckUpdateResponse::NoReleaseForChannel(ch)) => {
            eprintln!("No release on channel {ch}");
        }
        Response::CheckUpdate(command::CheckUpdateResponse::VpnNotConnected) => {
            eprintln!(
                "{}: checking for updates without an active VPN connection is insecure",
                CheckUpdateErrorKind::VpnNotConnected
            );
        }
        Response::CheckUpdate(command::CheckUpdateResponse::IntegrityError(msg)) => {
            eprintln!("{}: {msg}", CheckUpdateErrorKind::IntegrityError);
        }
        Response::CheckUpdate(command::CheckUpdateResponse::Error(msg)) => {
            eprintln!("Update check error: {msg}");
        }
        Response::StartUpdateRejected(msg) => {
            eprintln!("Update rejected: {msg}");
        }
        Response::UpdateStatus(status) => {
            println!("{status}");
        }
        Response::Version(v) => {
            println!("{v}");
        }
        // Internal response sent by the root process to itself when a WAN interface change
        // triggers a HOPR session reconnect. Never issued in response to a ctl command.
        Response::ForceReconnectAcknowledged => {}
    }
}

fn format_probability(p: f64) -> String {
    let s = format!("{:.8}", p);
    let trimmed = s.trim_end_matches('0');
    trimmed.trim_end_matches('.').to_string()
}

fn find_capacity<'a>(
    allocations: &'a [balance::CapacityEntry],
    allocator: &balance::CapacityAllocator,
) -> Option<&'a balance::Capacity> {
    allocations
        .iter()
        .find(|e| &e.allocator == allocator)
        .map(|e| &e.capacity)
}

fn format_capacity(capacity: Option<&balance::Capacity>) -> String {
    match capacity {
        None => String::new(),
        Some(c) => format!(
            " [{} msgs, {}]",
            human_msgs(c.expected_messages),
            human_bytes(c.byte_capacity)
        ),
    }
}

fn human_bytes(bytes: u64) -> String {
    const KB: u64 = 1_024;
    const MB: u64 = 1_024 * KB;
    const GB: u64 = 1_024 * MB;
    match bytes {
        b if b >= GB => format!("{:.1} GB", b as f64 / GB as f64),
        b if b >= MB => format!("{:.1} MB", b as f64 / MB as f64),
        b if b >= KB => format!("{:.1} KB", b as f64 / KB as f64),
        b => format!("{b} B"),
    }
}

fn human_msgs(msgs: u64) -> String {
    const K: u64 = 1_000;
    const M: u64 = 1_000 * K;
    const G: u64 = 1_000 * M;
    match msgs {
        m if m >= G => format!("{:.1}B", m as f64 / G as f64),
        m if m >= M => format!("{:.1}M", m as f64 / M as f64),
        m if m >= K => format!("{:.1}K", m as f64 / K as f64),
        m => format!("{m}"),
    }
}

fn determine_exitcode(resp: &Response) -> ExitCode {
    match resp {
        Response::Connect(command::ConnectResponse::AlreadyConnected(..)) => exitcode::OK,
        Response::Connect(command::ConnectResponse::Connecting(..)) => exitcode::OK,
        Response::Connect(command::ConnectResponse::DestinationNotFound) => exitcode::UNAVAILABLE,
        Response::Connect(command::ConnectResponse::WaitingToConnect(..)) => exitcode::OK,
        Response::Connect(command::ConnectResponse::UnableToConnect(..)) => exitcode::UNAVAILABLE,
        Response::Disconnect(command::DisconnectResponse::Disconnecting(..)) => exitcode::OK,
        Response::Disconnect(command::DisconnectResponse::NotConnected) => exitcode::PROTOCOL,
        Response::Status(..) => exitcode::OK,
        Response::Balance(Ok(..)) => exitcode::OK,
        Response::Balance(Err(..)) => exitcode::SOFTWARE,
        Response::Pong => exitcode::OK,
        Response::Telemetry(Some(_)) => exitcode::OK,
        Response::Telemetry(None) => exitcode::UNAVAILABLE,
        Response::NerdStats(command::NerdStatsResponse::NoInfo(command::TicketStatsStatus::Available(_))) => {
            exitcode::OK
        }
        Response::NerdStats(command::NerdStatsResponse::NoInfo(command::TicketStatsStatus::Waiting)) => {
            exitcode::UNAVAILABLE
        }
        Response::NerdStats(command::NerdStatsResponse::NoInfo(command::TicketStatsStatus::Error(_))) => {
            exitcode::SOFTWARE
        }
        Response::NerdStats(command::NerdStatsResponse::Connecting(..)) => exitcode::OK,
        Response::NerdStats(command::NerdStatsResponse::Connected(..)) => exitcode::OK,
        Response::FundingTool(command::FundingToolResponse::WrongPhase) => exitcode::UNAVAILABLE,
        Response::FundingTool(command::FundingToolResponse::Started) => exitcode::OK,
        Response::FundingTool(command::FundingToolResponse::InProgress) => exitcode::OK,
        Response::FundingTool(command::FundingToolResponse::Done) => exitcode::OK,
        Response::Info(..) => exitcode::OK,
        Response::StartClient(command::StartClientResponse::Started) => exitcode::OK,
        Response::StartClient(command::StartClientResponse::AlreadyRunning) => exitcode::PROTOCOL,
        Response::StopClient(command::StopClientResponse::Stopped) => exitcode::OK,
        Response::StopClient(command::StopClientResponse::NotRunning) => exitcode::PROTOCOL,
        Response::Destinations(..) => exitcode::OK,
        Response::WorkerOffline => exitcode::UNAVAILABLE,
        Response::CheckUpdate(command::CheckUpdateResponse::UpToDate { .. })
        | Response::CheckUpdate(command::CheckUpdateResponse::Available { .. }) => exitcode::OK,
        Response::CheckUpdate(command::CheckUpdateResponse::NoReleaseForChannel(_)) => exitcode::UNAVAILABLE,
        Response::CheckUpdate(command::CheckUpdateResponse::VpnNotConnected) => {
            CheckUpdateErrorKind::VpnNotConnected.exit_code()
        }
        Response::CheckUpdate(command::CheckUpdateResponse::IntegrityError(_)) => {
            CheckUpdateErrorKind::IntegrityError.exit_code()
        }
        Response::CheckUpdate(command::CheckUpdateResponse::Error(_)) => exitcode::SOFTWARE,
        Response::StartUpdateRejected(_) => exitcode::UNAVAILABLE,
        Response::UpdateStatus(_) => exitcode::OK,
        Response::Version(_) => exitcode::OK,
        // Internal response — see pretty_print for explanation
        Response::ForceReconnectAcknowledged => exitcode::PROTOCOL,
    }
}

fn print_ticket_stats_status(status: &command::TicketStatsStatus) {
    match status {
        command::TicketStatsStatus::Available(ts) => {
            let sci = balance::wxhopr_scientific(ts.ticket_price)
                .map(|s| format!(" ({s})"))
                .unwrap_or_default();
            println!(
                "Ticket Price: {}{}\nWinning Probability: {}",
                ts.ticket_price,
                sci,
                format_probability(ts.winning_probability)
            )
        }
        command::TicketStatsStatus::Waiting => {
            println!("waiting for incentive operations to become available")
        }
        command::TicketStatsStatus::Error(e) => eprintln!("Error fetching ticket stats: {e}"),
    }
}

fn print_nerd_stats(nerd_stats: &command::NerdStatsResponse) {
    match nerd_stats {
        command::NerdStatsResponse::NoInfo(ts_status) => {
            print_ticket_stats_status(ts_status);
            println!("(connect to a destination to see more stats)");
        }
        command::NerdStatsResponse::Connecting(ts_status, conn) => {
            print_ticket_stats_status(ts_status);
            println!("---");
            print_connecting_stats(conn);
        }
        command::NerdStatsResponse::Connected(ts_status, conn) => {
            print_ticket_stats_status(ts_status);
            println!("---");
            print_connected_stats(conn);
        }
    }
}

fn print_connecting_stats(stats: &command::ConnStats) {
    let mut str_resp = print_conn_stats_routing(stats, "-CONNECTING-");
    str_resp.push_str("---\n");
    str_resp.push_str(
        format!(
            "WireGuard Public Key: {}\n",
            stats.wg_pubkey.clone().unwrap_or("--pending generation--".to_string())
        )
        .as_str(),
    );
    str_resp.push_str(
        format!(
            "Assigned WireGuard IP: {}\n",
            stats.wg_ip.clone().unwrap_or("--pending registration--".to_string())
        )
        .as_str(),
    );
    if let Some(ref session) = stats.bridge_session {
        str_resp.push_str(&print_session(session));
    }
    str_resp.push_str(&print_session_or_pending(
        &stats.main_session,
        "--pending session creation--",
    ));
    str_resp.push_str(
        format!(
            "---\nExit WireGuard Public Key: {}\n",
            stats
                .wg_server_pubkey
                .clone()
                .unwrap_or("--pending registration--".to_string())
        )
        .as_str(),
    );
    println!("{str_resp}");
}

fn print_connected_stats(stats: &command::ConnStats) {
    let mut str_resp = print_conn_stats_routing(stats, "-o-");
    str_resp.push_str("---\n");
    if let Some(ref wg_pubkey) = stats.wg_pubkey {
        str_resp.push_str(format!("WireGuard Public Key: {}\n", wg_pubkey).as_str());
    }
    if let Some(ref ip) = stats.wg_ip {
        str_resp.push_str(format!("Assigned WireGuard IP: {ip}\n").as_str());
    }
    if let Some(ref session) = stats.bridge_session {
        str_resp.push_str(&print_session(session));
    }
    str_resp.push_str(&print_session_or_pending(&stats.main_session, "--none--"));
    if let Some(ref wg_pubkey) = stats.wg_server_pubkey {
        str_resp.push_str(format!("---\nExit WireGuard Public Key: {}\n", wg_pubkey).as_str());
    }
    println!("{str_resp}");
}

fn print_session(session: &command::ActiveSession) -> String {
    use command::ActiveSession;
    match session {
        ActiveSession::Bridge { bound_host, id } => {
            format!("Bridge Session entry: {bound_host}\nBridge Session ID: {id}\n")
        }
        ActiveSession::Ping { bound_host, id } => {
            format!("Ping Session entry: {bound_host}\nPing Session ID: {id}\n")
        }
        ActiveSession::Main { bound_host, id } => {
            format!("Main Session entry: {bound_host}\nMain Session ID: {id}\n")
        }
    }
}

fn print_session_or_pending(session: &Option<command::ActiveSession>, pending: &str) -> String {
    session
        .as_ref()
        .map(print_session)
        .unwrap_or_else(|| format!("Session entry: {pending}\nSession ID: {pending}\n"))
}

fn print_conn_stats_routing(stats: &command::ConnStats, title: &str) -> String {
    let mut str_resp = String::new();
    match stats.destination.routing.hop_count() {
        0 => {
            str_resp.push_str(&format!(
                "{node_addr}(me) -{title}-DIRECTLY--> {addr}({exit})\n",
                node_addr = stats.node_address.to_checksum(),
                addr = stats.destination.address.to_checksum(),
                exit = stats.destination.id,
            ));
        }
        1 => {
            str_resp.push_str(&format!(
                "{node_addr}(me) -{title}-VIA--1HOP--> {addr}({exit})\n",
                node_addr = stats.node_address.to_checksum(),
                addr = stats.destination.address.to_checksum(),
                exit = stats.destination.id,
            ));
        }
        nr => {
            str_resp.push_str(&format!(
                "{node_addr}(me) -{title}-VIA--{nr}HOPS--> {addr}({exit})\n",
                node_addr = stats.node_address.to_checksum(),
                addr = stats.destination.address.to_checksum(),
                exit = stats.destination.id,
            ));
        }
    }
    str_resp
}
