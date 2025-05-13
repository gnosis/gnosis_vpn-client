use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::thread;
use std::time;
use std::time::SystemTime;

use rand::Rng;
use reqwest::blocking;
use thiserror::Error;
use tracing::instrument;
use url::Url;

use gnosis_vpn_lib::command::Command;
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::connection::{self, Connection, Destination};
use gnosis_vpn_lib::entry_node::EntryNode;
use gnosis_vpn_lib::peer_id::PeerId;
use gnosis_vpn_lib::session::{self};
use gnosis_vpn_lib::state;
use gnosis_vpn_lib::{log_output, wireguard};

use crate::backoff;
use crate::backoff::FromIteratorToSeries;
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

#[derive(Debug)]
pub enum TargetState {
    Idle,
    Connect(Destination),
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
    #[error("Not yet implemented")]
    NotImplemented,
}

impl Core {
    pub fn init(config_path: &Path, sender: crossbeam_channel::Sender<Event>) -> Result<Core, Error> {
        let config = config::read(config_path)?;
        let mut state = state::read()?;
        let wireguard = match wireguard::best_flavor() {
            Ok(wg) => Some(wg),
            Err(e) => {
                tracing::error!(error = ?e, "could not determine wireguard handling mode - proceeding with manual wireguard interaction");
                None
            }
        };

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
            target_state: TargetState::Idle,
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
        match &self.target_state {
            TargetState::Connect(destination) => {
                match &self.connection {
                    Some(conn) => {
                        if conn.has_destination(&destination) {
                            tracing::info!(destination = %destination, "already connecting to target destination");
                        } else {
                            tracing::info!(current = %conn.destination(), target = %destination, "TODO disconnecting current destination")
                            // TODO
                        }
                    }
                    None => {
                        tracing::info!(destination = %destination, "TODO establishing new connection");
                        // TODO
                    }
                }
            }
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
            t => {
                tracing::info!("TODO {:?}", t);
                // TODO
            }
        }
    }

    fn setup_from_config(&mut self) -> Result<(), Error> {
        Ok(())
        /*
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
            */
    }

    #[instrument(level = tracing::Level::INFO, skip(self), ret(level = tracing::Level::DEBUG))]
    pub fn handle_cmd(&mut self, cmd: &Command) -> Result<Option<String>, Error> {
        match cmd {
            Command::Connect(peer_id) => match self.config.destinations().get(peer_id) {
                Some(dest) => {
                    tracing::info!(destination = %dest, "targetting new destination");
                    self.target_state = TargetState::Connect(dest.clone());
                    self.act_on_target();
                    Ok(Some(format!("targetting {}", dest)))
                }
                None => {
                    tracing::info!(peer_id = %peer_id, "destination peer id not found");
                    Ok(Some(format!("unknown peer id")))
                }
            },
            _ => Err(Error::NotImplemented),
        }
    }

    #[instrument(level = tracing::Level::INFO, skip(self), ret(level = tracing::Level::DEBUG))]
    pub fn handle_event(&mut self, event: Event) -> Result<(), Error> {
        match event {
            Event::ConnectWg(connection::ConnectInfo {
                endpoint,
                wg_registration,
            }) => {
                tracing::info!("trying wg conn");
                if let (Some(wg), Some(privkey)) = (&self.wg, self.state.wg_private_key()) {
                    tracing::info!("core received connected with wg_priv_key");
                    let interface_info = wireguard::InterfaceInfo {
                        private_key: privkey.clone(),
                        address: wg_registration.address(),
                        allowed_ips: None,
                        listen_port: None,
                    };
                    let peer_info = wireguard::PeerInfo {
                        public_key: wg_registration.server_public_key(),
                        endpoint,
                    };
                    let info = wireguard::ConnectSession::new(&interface_info, &peer_info);
                    let res = wg.connect_session(&info);
                    tracing::info!(?res, "res wg");
                }

                // self.establish_wg();
                Ok(())
            }
            Event::DisconnectWg => {
                // self.dismantle_wg();
                Ok(())
            }
        }
    }

    #[instrument(level = tracing::Level::INFO, skip(self), ret(level = tracing::Level::DEBUG))]
    pub fn update_config(&mut self, config_path: &Path) -> Result<(), Error> {
        let config = config::read(&config_path)?;
        self.config = config;
        // TODO
        //self.reset();
        Ok(())
    }

    fn establish_wg(&mut self) {
        // todo
        /*
                tracing::info!(wg = ?self.wg,
                    privkey = ?self.state.wg_private_key(),
                    port = ?self.connection.as_ref().and_then(|c| c.port().ok()),
                    conn = ?self.connection,
                    "foobar");
                // connect wireguard session if possible
                if let (Some(wg), Some(wg_conf), Some(privkey), Some(en_host), Some(port)) = (
                    &self.wg,
                    &self.config.wireguard,
                    &self.wg_priv_key(),
                    &self.config.hoprd_node.as_ref().and_then(|en| en.endpoint.host()),
                    &self.connection.as_ref().and_then(|c| c.port().ok()),
                ) {
                    let peer_info = wireguard::PeerInfo {
                        public_key: wg_conf.server_public_key.clone(),
                        endpoint: format!("{}:{}", en_host, port),
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
                }
            }

            fn dismantle_wg(&self) {
                if let Some(wg) = &self.wg {
                    match wg.close_session() {
                        Ok(_) => {
                            tracing::info!(
                                r"

            /---==========================---\
            |   VPN CONNECTION BROKEN        |
            \---==========================---/
        "
                            );
                        }
                        Err(e) => {
                            tracing::warn!(warn = ?e, "error closing wireguard connection");
                        }
                    }
                }
                */
    }
}
