use std::path::Path;
use std::thread;

use thiserror::Error;
use tracing::instrument;

use gnosis_vpn_lib::command::Command;
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::connection::{self, Connection, Destination};
use gnosis_vpn_lib::state;
use gnosis_vpn_lib::wireguard;

use crate::event::Event;

#[derive(Debug)]
pub struct Core {
    // configuration data
    config: Config,
    // global event transmitter
    sender: crossbeam_channel::Sender<Event>,
    // internal persistent application state
    state: state::State,
    // wg interface,
    wg: Option<Box<dyn wireguard::WireGuard>>,
    // shutdown event emitter
    shutdown_sender: Option<crossbeam_channel::Sender<()>>,

    connection: Option<connection::Connection>,
    target_state: TargetState,
}

#[derive(Clone, Debug)]
pub enum TargetState {
    Disconnected,
    Connect(Box<Destination>),
    Shutdown,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(#[from] config::Error),
    #[error("State error: {0}")]
    State(#[from] state::Error),
    #[error("WireGuard error: {0}")]
    WireGuard(#[from] wireguard::Error),
    #[error("Missing manual_mode configuration")]
    WireGuardManualModeMissing,
    #[error("Not yet implemented")]
    NotImplemented,
}

impl Core {
    pub fn init(config_path: &Path, sender: crossbeam_channel::Sender<Event>) -> Result<Core, Error> {
        let config = config::read(config_path)?;
        let wireguard = if config.wireguard().manual_mode.is_some() {
            tracing::info!("running in manual WireGuard mode, because of `manual_mode` entry in configuration file");
            None
        } else {
            match wireguard::best_flavor() {
                Ok(wg) => Some(wg),
                Err(e) => {
                    tracing::error!(error = ?e, "could not determine WireGuard handling mode");
                    print_manual_instructions();
                    return Err(Error::WireGuardManualModeMissing);
                }
            }
        };

        let mut state = match state::read() {
            Err(state::Error::NoFile) => {
                tracing::info!("no service state file found - clean start");
                Ok(state::State::default())
            }
            x => x,
        }?;

        if let (Some(wg), None) = (&wireguard, &state.wg_private_key()) {
            let priv_key = wg.generate_key()?;
            state.set_wg_private_key(priv_key.clone())?
        }

        let core = Core {
            config,
            state,
            wg: wireguard,
            sender,
            shutdown_sender: None,
            connection: None,
            target_state: TargetState::Disconnected,
        };
        Ok(core)
    }

    pub fn shutdown(&mut self) -> crossbeam_channel::Receiver<()> {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        self.shutdown_sender = Some(sender);
        self.target_state = TargetState::Shutdown;
        self.act_on_target();
        receiver
    }

    fn act_on_target(&mut self) {
        // avoid immutable borrow of self
        let target_state = self.target_state.clone();
        match target_state {
            TargetState::Connect(destination) => match &self.connection {
                Some(conn) => {
                    if conn.has_destination(&destination) {
                        tracing::info!(destination = %destination, "already connecting to target destination");
                    } else {
                        tracing::info!(current = %conn.destination(), target = %destination, "TODO disconnecting from current destination")
                    }
                }
                None => {
                    tracing::info!(destination = %destination, "establishing new connection");
                    self.connect(&destination);
                }
            },
            TargetState::Disconnected => match &mut self.connection {
                Some(conn) => {
                    tracing::info!(current = %conn.destination(), "disconnecting from current destination");
                    conn.dismantle();
                    if let Some(wg) = &self.wg {
                        match wg.close_session() {
                            Ok(_) => {
                                tracing::info!("WireGuard connection closed");
                            }
                            Err(err) => {
                                tracing::warn!(error = %err, "failed to close WireGuard connection");
                            }
                        }
                    }
                }
                None => tracing::info!("no connection to disconnect"),
            },
            TargetState::Shutdown => {
                match (&self.connection, &self.shutdown_sender) {
                    (Some(conn), _) => {
                        tracing::info!(current = %conn.destination(), "TODO disconnecting current destination");
                        // TODO
                    }
                    (None, Some(sender)) => {
                        tracing::info!("shutting down");
                        _ = sender.send(());
                    }
                    _ => {
                        tracing::warn!("shutdown target without shutdown sender");
                    }
                }
            }
        }
    }

    /*
    fn setup_from_config(&mut self) -> Result<(), Error> {
        Ok(())
        // self.check_close_session()?;
        if let (Some(entry_node), Some(session)) = (&self.config.hoprd_node, &self.config.connection) {
            let en_endpoint = entry_node.endpoint.clone();
            let en_api_token = entry_node.api_token.clone();
            let internal_port = entry_node.internal_connection_port.map(|port| format!(":{}", port));
            let en_listen_host = session.listen_host.clone().or(internal_port);
            let path = session.path.clone().unwrap_or_default();
            let en_path = match path {
                config::v1::SessionPathConfig::Hop(hop) => OldPath::Hop(hop),
                config::v1::SessionPathConfig::Intermediates(ids) => OldPath::Intermediates(ids.clone()),
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
                config::v1::SessionPathConfig::Hop(hop) => session::Path::Hop(hop),
                config::v1::SessionPathConfig::Intermediates(ids) => session::Path::Intermediates(ids.clone()),
            };

            let target_bridge = session::Target::Plain(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8000));
            let target_wg = session::Target::Plain(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 51821));

            let (s, r) = crossbeam_channel::bounded(1);
            let mut conn = Connection::new(
                &entry_node,
                xn_peer_id.to_string().as_str(),
                &path,
                &target_bridge,
                &target_wg,
                &wg_pub_key,
                s,
            );

            conn.establish();
            self.connection = Some(conn);
            let sender = self.sender.clone();
            thread::spawn(move || loop {
                crossbeam_channel::select! {
                recv(r) -> event => {
                            tracing::info!(event = ?event, "core received event");
                    match event {
                        Ok(connection::Event::Connected(conninfo)) => {
                            _ = sender.send(Event::ConnectWg(conninfo));
                            break;
                        }
                        Ok(connection::Event::Disconnected) => {
                            tracing::info!("sending disconnectwg");
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to receive event");
                            break;
                        }
                    }
                }
                            }
            });
        }
        Ok(())

    }
            */

    fn connect(&mut self, destination: &Destination) {
        let wg_pub_key = match self.wg_public_key() {
            Some(wg_pub_key) => wg_pub_key,
            None => {
                tracing::error!("Unable to create connection without WireGuard public key");
                tracing::error!(
                    ">> If you intend to use manual WireGuard mode, please set the public key in the configuration file: <<"
                );
                tracing::error!(">> [wireguard] <<");
                tracing::error!(r#">> manual_mode = {{ public_key = "<wg public key" }} <<"#);
                return;
            }
        };

        let (s, r) = crossbeam_channel::bounded(1);
        let mut conn = Connection::new(&self.config.entry_node(), destination, &wg_pub_key, s);
        conn.establish();
        self.connection = Some(conn);
        let sender = self.sender.clone();
        thread::spawn(move || loop {
            crossbeam_channel::select! {
                recv(r) -> event => {
                    match event {
                        Ok(connection::Event::Connected(conninfo)) => {
                            _ = sender.send(Event::ConnectWg(conninfo));
                            break;
                        }
                        Ok(connection::Event::Disconnected) => {
                            tracing::info!("connection hickup");
                        }
                        Ok(connection::Event::Dismantled) => {
                            tracing::info!("TODO connection dismantled");
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to receive event");
                            break;
                        }
                    }
                }
            }
        });
    }

    pub fn handle_cmd(&mut self, cmd: &Command) -> Result<Option<String>, Error> {
        tracing::info!(%cmd, "handling command");
        match cmd {
            Command::Connect(peer_id) => match self.config.destinations().get(peer_id) {
                Some(dest) => {
                    self.target_state = TargetState::Connect(Box::new(dest.clone()));
                    self.act_on_target();
                    Ok(Some(format!("targetting {}", dest)))
                }
                None => {
                    tracing::info!(peer_id = %peer_id, "cannot connect to destination - peer id not found");
                    Ok(Some("unknown peer id".to_string()))
                }
            },
            Command::Disconnect => {
                self.target_state = TargetState::Disconnected;
                self.act_on_target();
                Ok(Some("disconnecting".to_string()))
            }
            _ => Err(Error::NotImplemented),
        }
    }

    pub fn handle_event(&mut self, event: Event) -> Result<(), Error> {
        tracing::info!(%event, "handling event");
        match event {
            Event::ConnectWg(conninfo) => self.on_session_ready(conninfo),
        }
    }

    #[instrument(level = tracing::Level::INFO, skip(self), ret(level = tracing::Level::DEBUG))]
    pub fn update_config(&mut self, config_path: &Path) -> Result<(), Error> {
        _ = config::read(config_path)?;
        // self.config = config;
        Err(Error::NotImplemented)
    }

    fn on_session_ready(&mut self, conninfo: connection::ConnectInfo) -> Result<(), Error> {
        if let (Some(wg), Some(privkey)) = (&self.wg, self.state.wg_private_key()) {
            // automatic wg connection
            tracing::info!("iniating wireguard connection");
            let interface_info = wireguard::InterfaceInfo {
                private_key: privkey.clone(),
                address: conninfo.registration.address(),
                allowed_ips: None,
                listen_port: self.config.wireguard().listen_port,
            };
            let peer_info = wireguard::PeerInfo {
                public_key: conninfo.registration.server_public_key(),
                endpoint: conninfo.endpoint,
            };
            let connect_session = wireguard::ConnectSession::new(&interface_info, &peer_info);

            match wg.connect_session(&connect_session) {
                Ok(_) => {
                    tracing::info!("established wireguard connection");
                    tracing::info!(
                        r"

            /---==========================---\
            |   VPN CONNECTION ESTABLISHED   |
            \---==========================---/

            route: {}
        ",
                        self.connection
                            .as_ref()
                            .map(|c| c.pretty_print_path())
                            .unwrap_or("<unknown>".to_string())
                    );
                    Ok(())
                }
                Err(e) => {
                    tracing::warn!(warn = ?e, "failed to establish wireguard connection");
                    Err(Error::WireGuard(e))
                }
            }
        } else {
            // manual wg connection
            let interface_info = wireguard::InterfaceInfo {
                private_key: "<WireGuard private key>".to_string(),
                address: conninfo.registration.address(),
                allowed_ips: None,
                listen_port: self.config.wireguard().listen_port,
            };
            let peer_info = wireguard::PeerInfo {
                public_key: conninfo.registration.server_public_key(),
                endpoint: conninfo.endpoint,
            };
            let connect_session = wireguard::ConnectSession::new(&interface_info, &peer_info);
            tracing::info!(
                r"

            /---============================---\
            |   HOPRD CONNECTION ESTABLISHED   |
            \---============================---/

            route: {}

            --- ready for manual WireGuard connection (wg-quick configuration blueprint) ---

{}

            ",
                self.connection
                    .as_ref()
                    .map(|c| c.pretty_print_path())
                    .unwrap_or("<unknown>".to_string()),
                connect_session.to_file_string()
            );
            Ok(())
        }
    }

    fn wg_public_key(&self) -> Option<String> {
        if let (Some(wg), Some(privkey)) = (&self.wg, &self.state.wg_private_key()) {
            match wg.public_key(privkey.as_str()) {
                Ok(pubkey) => Some(pubkey),
                Err(e) => {
                    tracing::error!(error = %e, "Unable to generate public key from private key");
                    None
                }
            }
        } else {
            self.config.wireguard().manual_mode.map(|mm| mm.public_key)
        }
    }
}

fn print_manual_instructions() {
    tracing::error!(
        ">> If you intend to use manual WireGuard mode, please set the public key in the configuration file: <<"
    );
    tracing::error!(">> [wireguard] <<");
    tracing::error!(r#">> manual_mode = {{ public_key = "<wg public key" }} <<"#);
}
