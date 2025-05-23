use std::path::Path;
use std::thread;

use thiserror::Error;

use gnosis_vpn_lib::command::{self, Command, Response};
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::connection::{self, Connection, Destination};
use gnosis_vpn_lib::state::{self, State};
use gnosis_vpn_lib::wireguard::{self, WireGuard};

use crate::event::Event;

#[derive(Debug)]
pub struct Core {
    // configuration data
    config: Config,
    // global event transmitter
    sender: crossbeam_channel::Sender<Event>,
    // internal persistent application state
    state: State,
    // wg interface, will be None if manual mode is used
    wg: Option<Box<dyn WireGuard>>,
    // shutdown event emitter
    shutdown_sender: Option<crossbeam_channel::Sender<()>>,

    connection: Option<connection::Connection>,
    session_connected: bool,
    wg_connected: bool,
    target_destination: Option<Destination>,
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
}

struct ConfigSetup {
    state: State,
    config: Config,
    wg: Option<Box<dyn WireGuard>>,
}

impl Core {
    pub fn init(config_path: &Path, sender: crossbeam_channel::Sender<Event>) -> Result<Core, Error> {
        let cs = setup_from_config(config_path)?;

        Ok(Core {
            config: cs.config,
            state: cs.state,
            wg: cs.wg,
            sender,
            shutdown_sender: None,
            connection: None,
            session_connected: false,
            wg_connected: false,
            target_destination: None,
        })
    }

    pub fn shutdown(&mut self) -> crossbeam_channel::Receiver<()> {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        self.shutdown_sender = Some(sender);
        match &mut self.connection {
            Some(conn) => {
                tracing::info!(current = %conn.destination(), "disconnecting from current destination due to shutdown");
                self.target_destination = None;
                conn.dismantle();
                self.disconnect_wg();
            }
            None => {
                tracing::debug!("direct shutdown - no connection to disconnect");
                if let Some(s) = self.shutdown_sender.as_ref() {
                    _ = s.send(());
                };
            }
        }
        receiver
    }

    pub fn handle_cmd(&mut self, cmd: &Command) -> Result<Response, Error> {
        tracing::debug!(%cmd, "handling command");
        match cmd {
            Command::Connect(peer_id) => match self.config.destinations().get(peer_id) {
                Some(dest) => {
                    self.target_destination = Some(dest.clone());
                    self.act_on_target();
                    Ok(Response::connect(command::ConnectResponse::new(dest.clone().into())))
                }
                None => {
                    tracing::info!(peer_id = %peer_id, "cannot connect to destination - peer id not found");
                    Ok(Response::connect(command::ConnectResponse::peer_id_not_found()))
                }
            },
            Command::Disconnect => {
                self.target_destination = None;
                self.act_on_target();
                let conn = self.connection.clone();
                match conn {
                    Some(c) => Ok(Response::disconnect(command::DisconnectResponse::new(
                        c.destination().clone().into(),
                    ))),
                    None => Ok(Response::disconnect(command::DisconnectResponse::not_connected())),
                }
            }
            Command::Status => {
                let wg_status = self
                    .wg
                    .as_ref()
                    .map(|_| command::WireGuardStatus::new(self.wg_connected))
                    .unwrap_or(command::WireGuardStatus::manual());
                let status = match (
                    self.target_destination.clone(),
                    self.connection.clone().map(|c| c.destination()),
                    self.session_connected,
                ) {
                    (Some(dest), _, true) => command::Status::connected(dest.clone().into()),
                    (Some(dest), _, false) => command::Status::connecting(dest.clone().into()),
                    (None, Some(conn_dest), _) => command::Status::disconnecting(conn_dest.into()),
                    (None, None, _) => command::Status::disconnected(),
                };

                let destinations = self.config.destinations();
                Ok(Response::status(command::StatusResponse::new(
                    wg_status,
                    status,
                    destinations
                        .values()
                        .map(|v| {
                            let dest = v.clone();
                            dest.into()
                        })
                        .collect(),
                )))
            }
        }
    }

    pub fn handle_event(&mut self, event: Event) -> Result<(), Error> {
        tracing::debug!(%event, "handling event");
        match event {
            Event::ConnectWg(conninfo) => self.on_session_ready(conninfo),
            Event::Disconnected => self.on_session_disconnect(),
            Event::DropConnection => self.on_drop_connection(),
        }
    }

    pub fn update_config(&mut self, config_path: &Path) -> Result<(), Error> {
        let cs = setup_from_config(config_path)?;
        self.config = cs.config;
        self.state = cs.state;
        self.wg = cs.wg;
        Ok(())
    }

    fn act_on_target(&mut self) {
        match (self.target_destination.clone(), &mut self.connection) {
            (Some(dest), Some(conn)) => {
                if conn.has_destination(&dest) {
                    tracing::info!(destination = %dest, "already connecting to target destination");
                } else {
                    tracing::info!(current = %conn.destination(), target = %dest, "disconnecting from current destination to connect to target destination");
                    conn.dismantle();
                    self.disconnect_wg();
                }
            }
            (None, Some(conn)) => {
                tracing::info!(current = %conn.destination(), "disconnecting from current destination");
                conn.dismantle();
                self.disconnect_wg();
            }
            (Some(dest), None) => {
                tracing::info!(destination = %dest, "establishing new connection");
                self.connect(&dest);
            }
            (None, None) => {}
        };
    }

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
                            _ = sender.send(Event::ConnectWg(conninfo)).map_err(|error| {
                                tracing::error!(error = %error, "failed to send ConnectWg event");
                            });
                        }
                        Ok(connection::Event::Disconnected) => {
                            _ = sender.send(Event::Disconnected).map_err(|error| {
                                tracing::error!(error = %error, "failed to send Disconnected event");
                            });
                        }
                        Ok(connection::Event::Dismantled) => {
                            _ = sender.send(Event::DropConnection).map_err(|error| {
                                tracing::error!(error = %error, "failed to send DropConnection event");
                            });
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to receive event");
                        }
                    }
                }
            }
        });
    }

    fn on_session_ready(&mut self, conninfo: connection::ConnectInfo) -> Result<(), Error> {
        tracing::debug!(?conninfo, "on session ready");
        if self.session_connected {
            tracing::info!("reconnecting previously connected session - might be connection hiccup");
            return Ok(());
        }
        self.session_connected = true;
        if self.wg_connected {
            tracing::debug!("WireGuard connection already established");
            return Ok(());
        }
        if let (Some(wg), Some(privkey)) = (&self.wg, self.state.wg_private_key()) {
            // automatic wg connection
            tracing::debug!("initiating WireGuard connection");
            let interface_info = wireguard::InterfaceInfo {
                private_key: privkey.clone(),
                address: conninfo.registration.address(),
                allowed_ips: self.config.wireguard().allowed_ips,
                listen_port: self.config.wireguard().listen_port,
            };
            let peer_info = wireguard::PeerInfo {
                public_key: conninfo.registration.server_public_key(),
                endpoint: conninfo.endpoint,
            };
            let connect_session = wireguard::ConnectSession::new(&interface_info, &peer_info);

            match wg.connect_session(&connect_session) {
                Ok(_) => {
                    self.wg_connected = true;
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
                    tracing::warn!(warn = ?e, "failed to establish WireGuard connection");
                    Err(Error::WireGuard(e))
                }
            }
        } else {
            // manual wg connection
            let interface_info = wireguard::InterfaceInfo {
                private_key: "<WireGuard private key>".to_string(),
                address: conninfo.registration.address(),
                allowed_ips: self.config.wireguard().allowed_ips,
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

    fn on_session_disconnect(&mut self) -> Result<(), Error> {
        tracing::info!("session hickup detected - reconnecting");
        self.session_connected = false;
        Ok(())
    }

    fn on_drop_connection(&mut self) -> Result<(), Error> {
        tracing::debug!("on drop connection");
        self.session_connected = false;
        self.connection = None;
        if let Some(sender) = self.shutdown_sender.as_ref() {
            tracing::debug!("shutting down after disconnecting");
            _ = sender.send(());
        } else {
            self.act_on_target();
        }
        Ok(())
    }

    fn wg_public_key(&self) -> Option<String> {
        self.config.wireguard().manual_mode.map(|mm| mm.public_key).or_else(|| {
            if let (Some(wg), Some(privkey)) = (&self.wg, &self.state.wg_private_key()) {
                match wg.public_key(privkey.as_str()) {
                    Ok(pubkey) => Some(pubkey),
                    Err(e) => {
                        tracing::error!(error = %e, "Unable to generate public key from private key");
                        None
                    }
                }
            } else {
                None
            }
        })
    }

    fn disconnect_wg(&mut self) {
        if let Some(wg) = &self.wg {
            match wg.close_session() {
                Ok(_) => {
                    self.wg_connected = false;
                    tracing::info!("WireGuard connection closed");
                }
                Err(err) => {
                    tracing::warn!(error = %err, "failed to close WireGuard connection");
                }
            }
        }
    }
}

fn setup_from_config(config_path: &Path) -> Result<ConfigSetup, Error> {
    let config = config::read(config_path)?;
    let wireguard = if config.wireguard().manual_mode.is_some() {
        tracing::warn!("running in manual WireGuard mode, because of `manual_mode` entry in configuration file");
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

    // only triggerd in WireGuard handling mode
    if let (Some(wg), None) = (&wireguard, &state.wg_private_key()) {
        let priv_key = wg.generate_key()?;
        state.set_wg_private_key(priv_key.clone())?;
    }

    // print destinations warning
    if config.destinations().is_empty() {
        tracing::warn!(">> No destinations found in configuration file <<");
        tracing::warn!(">> Please check https://gnosisvpn.com/servers for more information <<");
    }

    Ok(ConfigSetup {
        state,
        config,
        wg: wireguard,
    })
}

fn print_manual_instructions() {
    tracing::error!(
        ">> If you intend to use manual WireGuard mode, please set the public key in the configuration file: <<"
    );
    tracing::error!(">> [wireguard] <<");
    tracing::error!(r#">> manual_mode = {{ public_key = "<wg public key" }} <<"#);
}
