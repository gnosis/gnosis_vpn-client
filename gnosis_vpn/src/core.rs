use std::path::Path;
use std::thread;

use thiserror::Error;

use gnosis_vpn_lib::command::{self, Command, Response};
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::connection::{self, Connection, Destination};
use gnosis_vpn_lib::log_output;

use crate::event::Event;

#[derive(Debug)]
pub struct Core {
    // configuration data
    config: Config,
    // global event transmitter
    sender: crossbeam_channel::Sender<Event>,
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
            Command::Ping => Ok(Response::Pong),
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
            Event::ConnectWg(conninfo) => self.on_session_ready(),
            Event::Disconnected(ping_has_worked) => self.on_session_disconnect(ping_has_worked),
            Event::DropConnection => self.on_drop_connection(),
        }
    }

    pub fn update_config(&mut self, config_path: &Path) -> Result<(), Error> {
        let cs = setup_from_config(config_path)?;
        self.config = cs.config;
        self.state = cs.state;
        self.wg = cs.wg;
        if let Some(conn) = &mut self.connection {
            tracing::info!(current = %conn.destination(), "disconnecting from current destination due to configuration update");
            conn.dismantle();
            self.disconnect_wg();
        }
        if let Some(dest) = self.target_destination.as_ref() {
            if let Some(new_dest) = self.config.destinations().get(&dest.peer_id) {
                tracing::debug!(current = %dest, new = %new_dest, "target destination updated");
                self.target_destination = Some(new_dest.clone());
            } else {
                tracing::info!(current = %dest, "clearing target destination - not found in new configuration");
                self.target_destination = None;
            }
        }
        Ok(())
    }

    fn act_on_target(&mut self) -> Result<(), Error> {
        match (self.target_destination.clone(), &mut self.connection) {
            (Some(dest), Some(conn)) => {
                if conn.has_destination(&dest) {
                    tracing::info!(destination = %dest, "already connecting to target destination");
                } else {
                    tracing::info!(current = %conn.destination(), target = %dest, "disconnecting from current destination to connect to target destination");
                    conn.dismantle();
                }
                Ok(())
            }
            (None, Some(conn)) => {
                tracing::info!(current = %conn.destination(), "disconnecting from current destination");
                conn.dismantle();
                Ok(())
            }
            (Some(dest), None) => {
                tracing::info!(destination = %dest, "establishing new connection");
                self.connect(&dest)
            }
            (None, None) => Ok(()),
        };
    }

    fn connect(&mut self, destination: &Destination) -> Result<(), Error> {
        let (s, r) = crossbeam_channel::bounded(1);
        let mut conn = Connection::new(self.config.entry_node(), destination.clone(), wg_pub_key, s);
        conn.establish();
        self.connection = Some(conn);
        let sender = self.sender.clone();
        thread::spawn(move || {
            loop {
                crossbeam_channel::select! {
                    recv(r) -> event => {
                        match event {
                            Ok(connection::Event::Connected(conninfo)) => {
                                _ = sender.send(Event::ConnectWg(conninfo)).map_err(|error| {
                                    tracing::error!(error = %error, "failed to send ConnectWg event");
                                });
                            }
                            Ok(connection::Event::Disconnected(ping_has_worked)) => {
                                _ = sender.send(Event::Disconnected(ping_has_worked)).map_err(|error| {
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
            }
        });
    }

    fn on_session_ready(&mut self) -> Result<(), Error> {
        tracing::debug!("on session ready");
        self.session_connected = true;
        Ok(())
    }

    fn on_session_disconnect(&mut self, ping_has_worked: bool) -> Result<(), Error> {
        self.session_connected = false;
        if ping_has_worked {
            tracing::info!("session disconnected - might be connection hiccup");
        } else {
            tracing::warn!("session cannot send data");
        }
        Ok(())
    }

    fn on_drop_connection(&mut self) -> Result<(), Error> {
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
}

fn setup_from_config(config_path: &Path) -> Result<Config, Error> {
    let config = config::read(config_path)?;

    // print destinations warning
    if config.destinations().is_empty() {
        log_output::print_no_destinations();
    }

    Ok(config)
}
