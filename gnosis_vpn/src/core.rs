use anyhow::Result;
use gnosis_vpn_lib::command::Command;
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::connection::{self, Connection};
use gnosis_vpn_lib::entry_node::EntryNode;
use gnosis_vpn_lib::peer_id::PeerId;
use gnosis_vpn_lib::session::{self};
use gnosis_vpn_lib::state::{self, State};
use gnosis_vpn_lib::{log_output, wireguard};
use rand::Rng;
use reqwest::blocking;
use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time;
use std::time::SystemTime;
use tracing::instrument;
use url::Url;

use crate::backoff;
use crate::backoff::FromIteratorToSeries;
use crate::entry_node as old_entry_node;
use crate::entry_node::{EntryNode as OldEntryNode, Path as OldPath};
use crate::event::Event;
use crate::exit_node::ExitNode;
use crate::remote_data;
use crate::remote_data::RemoteData;
use crate::session as old_session;
use crate::session::Session as OldSession;

#[derive(Debug)]
pub struct Core {
    // http client
    client: blocking::Client,
    // configuration data
    config: Config,
    // event transmitter
    sender: crossbeam_channel::Sender<Event>,
    // potential non critial user visible errors
    issues: Vec<Issue>,
    // internal persistent application state
    state: state::State,
    // wg interface,
    wg: Option<Box<dyn wireguard::WireGuard>>,
    // random generator
    rng: rand::rngs::ThreadRng,
    // shutdown event emitter
    shutdown_sender: Option<crossbeam_channel::Sender<()>>,

    status: Status,
    entry_node: Option<OldEntryNode>,
    exit_node: Option<ExitNode>,
    fetch_data: FetchData,
    session: Option<OldSession>,

    connection: Option<connection::Connection>,
}

#[derive(Debug)]
struct FetchData {
    addresses: RemoteData,
    open_session: RemoteData,
    list_sessions: RemoteData,
    close_session: RemoteData,
}

#[derive(Debug)]
enum Status {
    Idle,
    OpeningSession {
        start_time: SystemTime,
    },
    MonitoringSession {
        start_time: SystemTime,
        cancel_sender: crossbeam_channel::Sender<()>,
    },
    ClosingSession {
        start_time: SystemTime,
    },
}

#[derive(Debug)]
enum Issue {
    Config(config::Error),
    State(state::Error),
    WireGuardInit(wireguard::Error),
    WireGuard(wireguard::Error),
}

fn read_config() -> (Config, Option<Issue>) {
    match config::read() {
        Ok(cfg) => {
            tracing::info!("read config without issues");
            (cfg, None)
        }
        Err(config::Error::NoFile) => {
            tracing::info!("no config - using default");
            (Config::default(), None)
        }
        Err(e) => {
            tracing::warn!(warn = ?e, "failed to read config file");
            (Config::default(), Some(Issue::Config(e)))
        }
    }
}

fn read_state() -> (State, Option<Issue>) {
    match state::read() {
        Ok(state) => (state, None),
        Err(state::Error::NoFile) => (State::default(), None),
        Err(e) => {
            tracing::warn!(warn = ?e, "failed to read state file");
            (State::default(), Some(Issue::State(e)))
        }
    }
}

impl Core {
    pub fn init(sender: crossbeam_channel::Sender<Event>) -> Core {
        let (config, conf_issue) = read_config();
        let mut issues = conf_issue.map(|i| vec![i]).unwrap_or(Vec::new());
        let (wg, wg_errors) = wireguard::best_flavor();
        let mut wg_issues = wg_errors.iter().map(|i| Issue::WireGuardInit(i.clone())).collect();
        issues.append(&mut wg_issues);
        let (state, state_issue) = read_state();
        if let Some(issue) = state_issue {
            issues.push(issue);
        }

        let mut core = Core {
            config,
            issues,
            status: Status::Idle,
            entry_node: None,
            exit_node: None,
            client: blocking::Client::new(),
            fetch_data: FetchData {
                addresses: RemoteData::NotAsked,
                open_session: RemoteData::NotAsked,
                list_sessions: RemoteData::NotAsked,
                close_session: RemoteData::NotAsked,
            },
            state,
            wg,
            rng: rand::thread_rng(),
            sender,
            session: None,
            shutdown_sender: None,
            connection: None,
        };
        core.setup();
        core
    }

    pub fn shutdown(&mut self) -> Result<crossbeam_channel::Receiver<()>> {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        self.shutdown_sender = Some(sender);
        self.check_close_session()?;
        Ok(receiver)
    }

    fn setup(&mut self) {
        self.setup_wg_priv_key();
        if let Err(e) = self.setup_from_config() {
            tracing::error!(error = ?e, "failed setup from config");
        }
    }

    fn wg_priv_key(&self) -> Option<String> {
        if let Some(key) = &self.config.wireguard.as_ref().and_then(|wg| wg.private_key.clone()) {
            return Some(key.clone());
        }
        if let Some(key) = &self.state.wg_private_key {
            return Some(key.clone());
        }
        None
    }

    fn setup_wg_priv_key(&mut self) {
        // if wg is available check private key
        // gengerate a new one if none
        if let (Some(wg), Some(_), None) = (&self.wg, &self.config.wireguard, &self.wg_priv_key()) {
            let priv_key = match wg.generate_key() {
                Ok(priv_key) => priv_key,
                Err(e) => {
                    tracing::error!(error = ?e, "failed to generate wireguard private key");
                    self.replace_issue(Issue::WireGuard(e));
                    return;
                }
            };
            match self.state.set_wg_private_key(priv_key.clone()) {
                Ok(_) => match wg.public_key(priv_key.as_str()) {
                    Ok(pub_key) => {
                        tracing::info!("****** Generated wireguard private key ******");
                        tracing::info!(public_key = %pub_key, "****** Use this pub_key for onboarding ******");
                    }
                    Err(e) => {
                        tracing::error!(error = ?e, "failed to generate wireguard public key");
                        self.replace_issue(Issue::WireGuard(e));
                    }
                },
                Err(e) => {
                    tracing::error!(error = ?e, "failed to write wireguard private key to state");
                    self.replace_issue(Issue::State(e));
                }
            };
        }
    }

    fn setup_from_config(&mut self) -> Result<()> {
        // self.check_close_session()?;
        if let (Some(entry_node), Some(session)) = (&self.config.hoprd_node, &self.config.connection) {
            let en_endpoint = entry_node.endpoint.clone();
            let en_api_token = entry_node.api_token.clone();
            let internal_port = entry_node.internal_connection_port.map(|port| format!(":{}", port));
            let en_listen_host = session.listen_host.clone().or(internal_port);
            let path = session.path.clone().unwrap_or_default();
            let en_path = match path {
                config::SessionPathConfig::Hop(hop) => OldPath::Hop(hop),
                config::SessionPathConfig::Intermediates(ids) => OldPath::Intermediates(ids.clone()),
            };
            let xn_peer_id = session.destination;

            // convert config to old application struture
            self.entry_node = Some(OldEntryNode::new(
                &en_endpoint,
                &en_api_token,
                en_listen_host.as_deref(),
                en_path,
            ));
            self.exit_node = Some(ExitNode { peer_id: xn_peer_id });

            self.fetch_data.addresses = RemoteData::Fetching {
                started_at: SystemTime::now(),
            };
            // self.fetch_addresses()?;
            // self.check_open_session()?;
        }

        let priv_key = self
            .wg_priv_key()
            .ok_or(anyhow::anyhow!("missing wireguard private key"))?;
        let wg_pub_key = self
            .wg
            .as_ref()
            .ok_or(anyhow::anyhow!("missing wg module"))?
            .public_key(priv_key.as_str())?;

        if let (Some(entry_node), Some(session)) = (&self.config.hoprd_node, &self.config.connection) {
            let internal_port = entry_node.internal_connection_port.map(|port| format!(":{}", port));
            let en_listen_host = session.listen_host.clone().or(internal_port);
            let entry_node = EntryNode {
                endpoint: entry_node.endpoint.clone(),
                api_token: entry_node.api_token.clone(),
                listen_host: en_listen_host,
            };
            let xn_peer_id = session.destination;

            let en_path = session.path.clone().unwrap_or_default();
            let path = match en_path {
                config::SessionPathConfig::Hop(hop) => session::Path::Hop(hop),
                config::SessionPathConfig::Intermediates(ids) => session::Path::Intermediates(ids.clone()),
            };

            let target_bridge = session::Target::Plain(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8000));
            let target_wg = session::Target::Plain(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 51820));

            let mut conn = Connection::new(
                &entry_node,
                xn_peer_id.to_string().as_str(),
                &path,
                &target_bridge,
                &target_wg,
                &wg_pub_key,
            );

            conn.start();
            self.connection = Some(conn);
        }
        Ok(())
    }

    #[instrument(level = tracing::Level::INFO, skip(self), ret(level = tracing::Level::DEBUG))]
    pub fn handle_cmd(&mut self, cmd: &Command) -> Result<Option<String>> {
        match cmd {
            Command::Status => Ok(self.status()),
            Command::EntryNode {
                endpoint,
                api_token,
                listen_host,
                hop,
                intermediate_id,
            } => self.entry_node(endpoint, api_token, listen_host, hop, intermediate_id),
            Command::ExitNode { peer_id } => self.exit_node(peer_id),
        }
    }

    #[instrument(level = tracing::Level::INFO, skip(self), ret(level = tracing::Level::DEBUG))]
    pub fn handle_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::FetchAddresses(evt) => self.evt_fetch_addresses(evt),
            Event::FetchOpenSession(evt) => self.evt_fetch_open_session(evt),
            Event::FetchListSessions(evt) => self.evt_fetch_list_sessions(evt),
            Event::FetchCloseSession(evt) => self.evt_fetch_close_session(evt),
            Event::CheckSession => self.evt_check_session(),
        }
    }

    #[instrument(level = tracing::Level::INFO, skip(self), ret(level = tracing::Level::DEBUG))]
    pub fn update_config(&mut self) {
        tracing::info!("update config");
        let (config, issue) = read_config();
        self.config = config;
        if let Some(issue) = issue {
            self.replace_issue(issue);
        }
        self.setup();
    }

    fn replace_issue(&mut self, issue: Issue) {
        // remove existing config issue
        self.issues.retain(|i| {
            !matches!(
                (i, &issue),
                (Issue::Config(_), Issue::Config(_))
                    | (Issue::WireGuard(_), Issue::WireGuard(_))
                    | (Issue::State(_), Issue::State(_))
            )
        });
        self.issues.push(issue);
    }

    fn evt_fetch_addresses(&mut self, evt: remote_data::Event) -> Result<()> {
        match evt {
            remote_data::Event::Response(value) => {
                self.fetch_data.addresses = RemoteData::Success;
                match &mut self.entry_node {
                    Some(en) => {
                        let addresses = serde_json::from_value::<old_entry_node::Addresses>(value)?;
                        en.addresses = Some(addresses);
                        tracing::info!("fetched addresses");
                        Ok(())
                    }
                    None => {
                        tracing::warn!("unexpected internal state: no entry node");
                        anyhow::bail!("unexpected internal state: no entry node");
                    }
                }
            }
            remote_data::Event::Error(err) => match &self.fetch_data.addresses {
                RemoteData::RetryFetching {
                    backoffs: old_backoffs, ..
                } => {
                    let mut backoffs = old_backoffs.clone();
                    self.repeat_fetch_addresses(err, &mut backoffs);
                    tracing::warn!("retrying fetch addresses");
                    Ok(())
                }
                RemoteData::Fetching { .. } => {
                    let mut backoffs = backoff::get_addresses().to_vec();
                    self.repeat_fetch_addresses(err, &mut backoffs);
                    tracing::info!("retrying fetch addresses");
                    Ok(())
                }
                _ => {
                    tracing::warn!("unexpected internal state: remote data result while not fetching");
                    anyhow::bail!("unexpected internal state: remote data result while not fetching");
                }
            },
            remote_data::Event::Retry => self.fetch_addresses(),
        }
    }

    fn evt_fetch_open_session(&mut self, evt: remote_data::Event) -> Result<()> {
        match evt {
            remote_data::Event::Response(value) => {
                let session = serde_json::from_value::<OldSession>(value)?;
                let session_port = session.port;
                self.session = Some(session);
                self.fetch_data.open_session = RemoteData::Success;
                let next_check = self.rng.gen_range(5..13);
                let cancel_sender =
                    old_session::schedule_check_session(time::Duration::from_secs(next_check), &self.sender);
                self.status = Status::MonitoringSession {
                    start_time: SystemTime::now(),
                    cancel_sender,
                };

                // prepare session path for printing
                let session_path = {
                    let (en, path) = match &self.entry_node {
                        Some(en) => (en.to_string(), en.path.to_string()),
                        None => ("<entry_node>".to_string(), "<path>".to_string()),
                    };

                    let xn = match &self.config.connection {
                        Some(conn) => match conn.target.as_ref().and_then(|t| t.host.clone()) {
                            Some(host) => format!(
                                "({})({})",
                                log_output::peer_id(conn.destination.to_string().as_str()),
                                host
                            ),
                            None => format!("({})", log_output::peer_id(conn.destination.to_string().as_str())),
                        },
                        None => "<exitnode>".to_string(),
                    };

                    if path.is_empty() {
                        format!("{} <-> {}", en, xn)
                    } else {
                        format!("{} <-> {} <-> {}", en, path, xn)
                    }
                };

                // connect wireguard session if possible
                if let (Some(wg), Some(_), Some(privkey), Some(wg_conf), Some(en_host)) = (
                    &self.wg,
                    &self.config.wireguard,
                    &self.wg_priv_key(),
                    &self.config.wireguard,
                    &self.config.hoprd_node.as_ref().and_then(|en| en.endpoint.host()),
                ) {
                    let peer_info = wireguard::PeerInfo {
                        public_key: wg_conf.server_public_key.clone(),
                        endpoint: format!("{}:{}", en_host, session_port),
                    };
                    let interface_info = wireguard::InterfaceInfo {
                        private_key: privkey.clone(),
                        address: wg_conf.address.clone(),
                        allowed_ips: wg_conf.allowed_ips.clone(),
                        listen_port: wg_conf.listen_port,
                    };
                    let info = wireguard::ConnectSession {
                        interface: interface_info,
                        peer: peer_info,
                    };

                    match wg.connect_session(&info) {
                        Ok(_) => {
                            tracing::info!("opened session and wireguard connection");
                            tracing::info!(
                                r"

    /---==========================---\
    |   VPN CONNECTION ESTABLISHED   |
    \---==========================---/

    route: {}
",
                                session_path
                            );
                        }
                        Err(e) => {
                            tracing::warn!(warn = ?e, "openend session but failed to connect wireguard session");
                            self.replace_issue(Issue::WireGuard(e));
                        }
                    }
                } else {
                    tracing::info!("opened session without handling wireguard");

                    tracing::info!(
                        r"

    /---============================---\
    |   HOPRD CONNECTION ESTABLISHED   |
    \---============================---/

    route: {}
",
                        session_path
                    );
                }

                Ok(())
            }
            remote_data::Event::Error(err) => match &self.fetch_data.open_session {
                RemoteData::RetryFetching {
                    backoffs: old_backoffs, ..
                } => {
                    let mut backoffs = old_backoffs.clone();
                    self.repeat_fetch_open_session(err, &mut backoffs);
                    tracing::warn!("retrying open session");
                    Ok(())
                }
                RemoteData::Fetching { .. } => {
                    let mut backoffs = backoff::open_session().to_vec();
                    self.repeat_fetch_open_session(err, &mut backoffs);
                    tracing::info!("retrying open session");
                    Ok(())
                }
                _ => {
                    tracing::warn!("unexpected internal state: remote data result while not fetching");
                    anyhow::bail!("unexpected internal state: remote data result while not fetching");
                }
            },
            remote_data::Event::Retry => self.fetch_open_session(),
        }
    }

    fn evt_fetch_list_sessions(&mut self, evt: remote_data::Event) -> Result<()> {
        match evt {
            remote_data::Event::Response(value) => {
                self.fetch_data.list_sessions = RemoteData::Success;
                let res_sessions = serde_json::from_value::<Vec<old_session::Session>>(value);
                match res_sessions {
                    Ok(sessions) => self.verify_session(&sessions),
                    Err(e) => {
                        self.status = Status::Idle;
                        tracing::warn!(warn = ?e, "failed to parse sessions");
                        anyhow::bail!("stopped monitoring - failed to parse sessions: {}", e);
                    }
                }
            }
            remote_data::Event::Error(err) => match &self.fetch_data.list_sessions {
                RemoteData::RetryFetching {
                    backoffs: old_backoffs, ..
                } => {
                    let mut backoffs = old_backoffs.clone();
                    tracing::warn!("retrying list sessions");
                    self.repeat_fetch_list_sessions(err, &mut backoffs)
                }
                RemoteData::Fetching { .. } => {
                    let mut backoffs = backoff::list_sessions().to_vec();
                    tracing::info!("retrying list sessions");
                    self.repeat_fetch_list_sessions(err, &mut backoffs)
                }
                _ => {
                    tracing::warn!("unexpected internal state: remote data result while not fetching");
                    anyhow::bail!("unexpected internal state: remote data result while not fetching");
                }
            },
            remote_data::Event::Retry => self.fetch_list_sessions(),
        }
    }

    fn evt_fetch_close_session(&mut self, evt: remote_data::Event) -> Result<()> {
        match evt {
            remote_data::Event::Response(_) => {
                self.fetch_data.close_session = RemoteData::Success;
                self.status = Status::Idle;
                tracing::info!("closed session");
                if let Some(sender) = &self.shutdown_sender {
                    let res = sender.send(());
                    match res {
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(warn = ?e, "failed sending shutdown event after closing session");
                        }
                    }
                    Ok(())
                } else {
                    self.check_open_session()
                }
            }
            remote_data::Event::Error(err) => match &self.fetch_data.close_session {
                RemoteData::RetryFetching {
                    backoffs: old_backoffs, ..
                } => {
                    let mut backoffs = old_backoffs.clone();
                    tracing::warn!("retrying close session");
                    self.repeat_fetch_close_session(err, &mut backoffs)
                }
                RemoteData::Fetching { .. } => {
                    let mut backoffs = backoff::close_session().to_vec();
                    tracing::info!("retrying close session");
                    self.repeat_fetch_close_session(err, &mut backoffs)
                }
                _ => {
                    tracing::warn!("unexpected internal state: remote data result while not fetching");
                    anyhow::bail!("unexpected internal state: remote data result while not fetching");
                }
            },
            remote_data::Event::Retry => self.fetch_close_session(),
        }
    }

    fn evt_check_session(&mut self) -> Result<()> {
        match (&self.status, &self.fetch_data.list_sessions) {
            (_, RemoteData::Fetching { .. }) | (_, RemoteData::RetryFetching { .. }) => {
                tracing::info!("skipping session check because already ongoing");
                Ok(())
            }
            (Status::MonitoringSession { .. }, _) => {
                self.fetch_data.list_sessions = RemoteData::Fetching {
                    started_at: SystemTime::now(),
                };
                self.fetch_list_sessions()
            }
            _ => {
                tracing::warn!("skipping session check because not monitoring session");
                Ok(())
            }
        }
    }

    fn repeat_fetch_addresses(&mut self, error: remote_data::CustomError, backoffs: &mut Vec<time::Duration>) {
        if let Some(backoff) = backoffs.pop() {
            let cancel_sender = old_entry_node::schedule_retry_query_addresses(backoff, &self.sender);
            self.fetch_data.addresses = RemoteData::RetryFetching {
                error,
                cancel_sender,
                backoffs: backoffs.clone(),
            };
        } else {
            self.fetch_data.addresses = RemoteData::Failure { error };
        }
    }

    fn repeat_fetch_open_session(&mut self, error: remote_data::CustomError, backoffs: &mut Vec<time::Duration>) {
        if let Some(backoff) = backoffs.pop() {
            let cancel_sender = old_session::schedule_retry_open(backoff, &self.sender);
            self.fetch_data.open_session = RemoteData::RetryFetching {
                error,
                cancel_sender,
                backoffs: backoffs.clone(),
            };
        } else {
            self.fetch_data.open_session = RemoteData::Failure { error };
            self.status = Status::Idle;
        }
    }

    fn repeat_fetch_list_sessions(
        &mut self,
        error: remote_data::CustomError,
        backoffs: &mut Vec<time::Duration>,
    ) -> Result<()> {
        if let Some(backoff) = backoffs.pop() {
            let cancel_sender = old_entry_node::schedule_retry_list_sessions(backoff, &self.sender);
            self.fetch_data.list_sessions = RemoteData::RetryFetching {
                error,
                cancel_sender,
                backoffs: backoffs.clone(),
            };
            Ok(())
        } else {
            self.fetch_data.list_sessions = RemoteData::Failure { error };
            if let Status::MonitoringSession { .. } = self.status {
                self.check_close_session()
            } else {
                tracing::warn!("unexpected internal state: failed list session call while not monitoring session");
                anyhow::bail!("unexpected internal state: failed list session call while not monitoring session");
            }
        }
    }

    fn repeat_fetch_close_session(
        &mut self,
        error: remote_data::CustomError,
        backoffs: &mut Vec<time::Duration>,
    ) -> Result<()> {
        if let Some(backoff) = backoffs.pop() {
            let cancel_sender = old_session::schedule_retry_close(backoff, &self.sender);
            self.fetch_data.close_session = RemoteData::RetryFetching {
                error,
                cancel_sender,
                backoffs: backoffs.clone(),
            };
            Ok(())
        } else {
            self.fetch_data.close_session = RemoteData::Failure { error };
            if let Status::ClosingSession { .. } = self.status {
                self.status = Status::Idle;
                Ok(())
            } else {
                tracing::warn!("unexpected internal state: failed close session call while not closing session");
                anyhow::bail!("unexpected internal state: failed close session call while not closing session");
            }
        }
    }

    fn status(&self) -> Option<String> {
        tracing::info!("respond with status");
        Some(self.to_string())
    }

    fn entry_node(
        &mut self,
        endpoint: &Url,
        api_token: &str,
        listen_host: &Option<String>,
        hop: &Option<u8>,
        intermediate_id: &Option<PeerId>,
    ) -> Result<Option<String>> {
        self.check_close_session()?;

        // TODO move this to library and enhance CLI to only allow one option
        // hop has precedence over intermediate_id
        let path = match (hop, intermediate_id) {
            (Some(h), _) => OldPath::Hop(*h),
            (_, Some(id)) => OldPath::Intermediates(vec![*id]),
            _ => OldPath::Hop(1),
        };
        self.entry_node = Some(OldEntryNode::new(endpoint, api_token, listen_host.as_deref(), path));
        self.fetch_data.addresses = RemoteData::Fetching {
            started_at: SystemTime::now(),
        };
        self.fetch_addresses()?;
        self.check_open_session()?;
        tracing::info!("set entry node");
        Ok(None)
    }

    fn exit_node(&mut self, peer_id: &PeerId) -> Result<Option<String>> {
        self.check_close_session()?;
        self.exit_node = Some(ExitNode { peer_id: *peer_id });
        self.check_open_session()?;
        tracing::info!("set exit node");
        Ok(None)
    }

    #[instrument(level = tracing::Level::INFO, skip(self))]
    fn check_open_session(&mut self) -> Result<()> {
        if !matches!(&self.status, Status::Idle) {
            tracing::info!(status = ?self.status, "need Idle status to open session");
            return Ok(());
        }

        if self.entry_node.is_none() {
            tracing::info!("need hoprd node parameters to open session");
            return Ok(());
        }
        if self.config.connection.is_none() {
            tracing::info!("need connection parameters to open session");
            return Ok(());
        }
        self.status = Status::OpeningSession {
            start_time: SystemTime::now(),
        };
        self.fetch_data.open_session = RemoteData::Fetching {
            started_at: SystemTime::now(),
        };
        self.fetch_open_session()
    }

    fn check_close_session(&mut self) -> Result<()> {
        self.cancel_fetch_addresses();
        self.cancel_fetch_open_session();
        self.cancel_fetch_list_sessions();
        self.cancel_fetch_close_session();
        self.cancel_session_monitoring();
        match &self.status {
            Status::MonitoringSession { .. } => {
                self.status = Status::ClosingSession {
                    start_time: SystemTime::now(),
                };

                // close wireguard session if possible
                if let (Some(wg), Some(_)) = (&self.wg, &self.config.wireguard) {
                    match wg.close_session() {
                        Ok(_) => {
                            tracing::info!("closed wireguard connection");
                        }
                        Err(e) => {
                            tracing::warn!(warn = ?e, "error closing wireguard connection");
                            self.replace_issue(Issue::WireGuard(e));
                        }
                    }
                };

                // close hoprd session
                self.fetch_data.close_session = RemoteData::Fetching {
                    started_at: SystemTime::now(),
                };
                self.fetch_close_session()
            }
            _ => {
                if let Some(sender) = &self.shutdown_sender {
                    let res = sender.send(());
                    match res {
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(warn = ?e, "failed sending shutdown event after canceling ongoing requests");
                        }
                    };
                }
                Ok(())
            }
        }
    }

    fn fetch_addresses(&mut self) -> Result<()> {
        match &self.entry_node {
            Some(en) => {
                tracing::info!("querying addresses");
                en.query_addresses(&self.client, &self.sender)
            }
            _ => {
                tracing::warn!("no entry node to fetch addresses");
                Ok(())
            }
        }
    }

    fn fetch_open_session(&mut self) -> Result<()> {
        if let (Some(en), Some(session)) = (&self.entry_node, &self.config.connection) {
            let open_session = old_session::OpenSession {
                endpoint: en.endpoint.clone(),
                api_token: en.api_token.clone(),
                destination: session.destination.to_string(),
                listen_host: en.listen_host.clone(),
                path: session.path.clone(),
                target: session.target.clone(),
                capabilities: session.capabilities.clone(),
            };
            tracing::info!("opening session");
            old_session::open(&self.client, &self.sender, &open_session)?;
        } else {
            tracing::warn!("no entry node or session to open");
        }
        Ok(())
    }

    fn fetch_list_sessions(&mut self) -> Result<()> {
        match &self.entry_node {
            Some(en) => {
                tracing::info!("querying list sessions");
                en.list_sessions(&self.client, &self.sender)
            }
            _ => {
                tracing::warn!("no entry node to query list sessions");
                Ok(())
            }
        }
    }

    fn fetch_close_session(&mut self) -> Result<()> {
        match (&self.entry_node, &self.session) {
            (Some(en), Some(sess)) => {
                tracing::info!("closing session");
                sess.close(&self.client, &self.sender, en)
            }
            _ => {
                tracing::warn!("no entry node or session to close");
                Ok(())
            }
        }
    }

    fn verify_session(&mut self, sessions: &[old_session::Session]) -> Result<()> {
        match (&self.session, &self.status) {
            (Some(sess), Status::MonitoringSession { start_time, .. }) => {
                if sess.verify_open(sessions) {
                    tracing::info!(session = ?sess, since = log_output::elapsed(start_time), "verified session open");
                    let next_check = self.rng.gen_range(5..13);
                    let cancel_sender =
                        old_session::schedule_check_session(time::Duration::from_secs(next_check), &self.sender);
                    self.status = Status::MonitoringSession {
                        start_time: *start_time,
                        cancel_sender,
                    };
                    Ok(())
                } else {
                    tracing::warn!(session = ?sess, "session no longer open");
                    self.status = Status::Idle;
                    self.check_open_session()
                }
            }
            (Some(_sess), _) => {
                tracing::warn!("unexpected internal state: session verification while not monitoring session");
                anyhow::bail!("unexpected internal state: session verification while not monitoring session");
            }
            (None, _status) => {
                tracing::warn!("unexpected internal state: session verification while no session");
                anyhow::bail!("unexpected internal state: session verification while no session");
            }
        }
    }

    fn cancel_fetch_addresses(&self) {
        if let RemoteData::RetryFetching { cancel_sender, .. } = &self.fetch_data.addresses {
            let res = cancel_sender.send(());
            match res {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "failed sending cancel query addresses");
                }
            }
        }
    }

    fn cancel_fetch_open_session(&self) {
        if let RemoteData::RetryFetching { cancel_sender, .. } = &self.fetch_data.open_session {
            let res = cancel_sender.send(());
            match res {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "failed sending cancel open session");
                }
            }
        }
    }

    fn cancel_fetch_list_sessions(&self) {
        if let RemoteData::RetryFetching { cancel_sender, .. } = &self.fetch_data.list_sessions {
            let res = cancel_sender.send(());
            match res {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "failed sending cancel list sessions");
                }
            }
        }
    }

    fn cancel_fetch_close_session(&self) {
        if let RemoteData::RetryFetching { cancel_sender, .. } = &self.fetch_data.close_session {
            let res = cancel_sender.send(());
            match res {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "failed sending cancel close session");
                }
            }
        }
    }

    fn cancel_session_monitoring(&self) {
        if let Status::MonitoringSession { cancel_sender, .. } = &self.status {
            let res = cancel_sender.send(());
            match res {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "failed sending cancel monitoring session");
                }
            }
        }
    }
}

impl fmt::Display for ExitNode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let peer = self.peer_id.to_string();
        let print = HashMap::from([("peer_id", peer.as_str())]);
        let val = log_output::serialize(&print);
        write!(f, "{}", val)
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let val = match self {
            Status::Idle => "idle",
            Status::OpeningSession { start_time } => {
                &format!("opening session since {}", log_output::elapsed(start_time)).to_string()
            }
            Status::MonitoringSession { start_time, .. } => {
                &format!("monitoring session since {}", log_output::elapsed(start_time)).to_string()
            }
            Status::ClosingSession { start_time } => {
                &format!("closing session since {}", log_output::elapsed(start_time)).to_string()
            }
        };
        write!(f, "{}", val)
    }
}

impl fmt::Display for Core {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut print = HashMap::new();
        if self.config == Config::default() {
            print.insert("config", "<default>".to_string());
        }
        if !self.issues.is_empty() {
            print.insert(
                "issues",
                self.issues
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<String>>()
                    .join(" | "),
            );
        }
        let val = log_output::serialize(&print);
        write!(f, "{}", val)
    }
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let val = match self {
            Issue::Config(e) => format!("config file issue: {}", e),
            Issue::WireGuardInit(e) => format!("wireguard initialization issue: {}", e),
            Issue::State(e) => format!("storage issue: {}", e),
            Issue::WireGuard(e) => format!("wireguard issue: {}", e),
        };
        write!(f, "{}", val)
    }
}
