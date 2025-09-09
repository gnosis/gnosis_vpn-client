use std::path::Path;
use std::thread;

use thiserror::Error;

use gnosis_vpn_lib::command::{self, Command, Response};
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::connection::{self, Connection, destination::Destination};
use gnosis_vpn_lib::entry_node::EntryNode;
use gnosis_vpn_lib::node::{self, Node};
use gnosis_vpn_lib::{balance, info, log_output, wg_tooling};

use crate::event::Event;

#[derive(Debug)]
pub struct Core {
    // configuration data
    config: Config,
    // global event transmitter
    sender: crossbeam_channel::Sender<Event>,
    // shutdown event emitter
    shutdown_sender: Option<crossbeam_channel::Sender<()>>,

    // connection to exit
    connection: Option<connection::Connection>,
    // connection to entry
    node: Node,
    session_connected: bool,
    target_destination: Option<Destination>,

    // internal cancellation sender
    cancel_channel: (crossbeam_channel::Sender<Cancel>, crossbeam_channel::Receiver<Cancel>),

    // results from node
    balance: Option<balance::Balance>,
    info: Option<info::Info>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(#[from] config::Error),
    #[error("WireGuard error: {0}")]
    WgTooling(#[from] wg_tooling::Error),
}

enum Cancel {
    Node,
    Connection,
}

impl Core {
    pub fn init(config_path: &Path, sender: crossbeam_channel::Sender<Event>) -> Result<Core, Error> {
        let config = read_config(config_path)?;
        wg_tooling::available()?;

        let cancel_channel = crossbeam_channel::unbounded();
        let node = setup_node(config.entry_node(), sender.clone(), cancel_channel.1.clone());
        node.run();

        Ok(Core {
            config,
            sender,
            shutdown_sender: None,
            connection: None,
            node,
            session_connected: false,
            target_destination: None,
            balance: None,
            info: None,
            cancel_channel: crossbeam_channel::unbounded(),
        })
    }

    pub fn shutdown(&mut self) -> crossbeam_channel::Receiver<()> {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        self.shutdown_sender = Some(sender);
        _ = self.cancel_channel.0.send(Cancel::Node).map_err(|e| {
            tracing::error!(%e, "failed to send cancel to node");
        });
        match &mut self.connection {
            Some(conn) => {
                tracing::info!(current = %conn.destination(), "disconnecting from current destination due to shutdown");
                self.target_destination = None;
                conn.dismantle();
            }
            None => {
                tracing::debug!("direct shutdown - no connection to disconnect");
                _ = self.cancel_channel.0.send(Cancel::Connection).map_err(|e| {
                    tracing::error!(%e, "failed to send cancel to connection");
                });
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
            Command::Connect(address) => match self.config.destinations().get(address) {
                Some(dest) => {
                    self.target_destination = Some(dest.clone());
                    self.act_on_target()?;
                    Ok(Response::connect(command::ConnectResponse::new(dest.clone().into())))
                }
                None => {
                    tracing::info!(address = %address, "cannot connect to destination - address not found");
                    Ok(Response::connect(command::ConnectResponse::address_not_found()))
                }
            },
            Command::Disconnect => {
                self.target_destination = None;
                self.act_on_target()?;
                let dest = self.connection.as_ref().map(|c| c.destination());
                match dest {
                    Some(d) => Ok(Response::disconnect(command::DisconnectResponse::new(d.into()))),
                    None => Ok(Response::disconnect(command::DisconnectResponse::not_connected())),
                }
            }
            Command::Status => {
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
                let funding_issues = self.balance.as_ref().map(|b| b.prioritized_funding_issues());
                Ok(Response::status(command::StatusResponse::new(
                    status,
                    destinations
                        .values()
                        .map(|v| {
                            let dest = v.clone();
                            dest.into()
                        })
                        .collect(),
                    funding_issues.into(),
                    self.info.as_ref().into(),
                )))
            }
            Command::Balance => match &self.balance {
                Some(balance) => {
                    let issues = balance.prioritized_funding_issues();
                    let resp = command::BalanceResponse::new(
                        format!("{} xDai", balance.node_xdai),
                        format!("{} wxHOPR", balance.safe_wxhopr),
                        format!("{} wxHOPR", balance.channels_out_wxhopr),
                        issues,
                    );
                    Ok(Response::Balance(Some(resp)))
                }
                None => Ok(Response::Balance(None)),
            },
        }
    }

    pub fn handle_event(&mut self, event: Event) -> Result<(), Error> {
        tracing::debug!(%event, "handling event");
        match event {
            Event::ConnectionEvent(connection::Event::Connected) => self.on_connected(),
            Event::ConnectionEvent(connection::Event::Disconnected) => self.on_disconnected(),
            Event::ConnectionEvent(connection::Event::Broken) => self.on_broken(),
            Event::ConnectionEvent(connection::Event::Dismantled) => self.on_dismantled(),
            Event::NodeEvent(node::Event::Info(info)) => self.on_info(info),
            Event::NodeEvent(node::Event::Balance(balance)) => self.on_balance(balance),
            Event::NodeEvent(node::Event::BackoffExhausted) => self.on_inoperable_node(),
        }
    }

    pub fn update_config(&mut self, config_path: &Path) -> Result<(), Error> {
        let config = read_config(config_path)?;
        // handle existing connection
        if let Some(conn) = &mut self.connection {
            tracing::info!(current = %conn.destination(), "disconnecting from current destination due to configuration update");
            conn.dismantle();
        }
        // update target
        if let Some(dest) = self.target_destination.as_ref() {
            if let Some(new_dest) = config.destinations().get(&dest.address) {
                tracing::debug!(current = %dest, new = %new_dest, "target destination updated");
                self.target_destination = Some(new_dest.clone());
            } else {
                tracing::info!(current = %dest, "clearing target destination - not found in new configuration");
                self.target_destination = None;
            }
        }

        // handle existing node
        self.node.cancel();
        self.balance = None;
        self.info = None;

        // setup new node
        let node = setup_node(config.entry_node(), self.sender.clone(), self.cancel_channel.1.clone());
        node.run();

        self.config = config;
        self.node = node;
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
        }
    }

    fn connect(&mut self, destination: &Destination) -> Result<(), Error> {
        let (s, r) = crossbeam_channel::unbounded();
        let wg = wg_tooling::WireGuard::from_config(self.config.wireguard())?;
        let mut conn = Connection::new(
            self.config.entry_node(),
            destination.clone(),
            wg,
            s,
            self.config.connection(),
        );
        conn.establish();
        self.connection = Some(conn);
        let sender = self.sender.clone();
        thread::spawn(move || {
            loop {
                crossbeam_channel::select! {
                    recv(r) -> conn_event => {
                        match conn_event {
                            Ok(event) => {
                                _ = sender.send(Event::ConnectionEvent(event)).map_err(|error| {
                                    tracing::error!(%event, %error, "failed to send ConnectionEvent event");
                                });
                            }
                            Err(e) => {
                                tracing::warn!(error = ?e, "failed to receive event");
                            }
                        }
                    }
                }
            }
        });
        Ok(())
    }

    fn on_connected(&mut self) -> Result<(), Error> {
        tracing::debug!("connection ready");
        self.session_connected = true;
        Ok(())
    }

    fn on_disconnected(&mut self) -> Result<(), Error> {
        self.session_connected = false;
        tracing::info!("connection disconnected - might be network hiccup");
        Ok(())
    }

    fn on_broken(&mut self) -> Result<(), Error> {
        tracing::warn!("connection broken - attempting to reconnect");
        self.session_connected = false;
        match self.connection.as_mut() {
            Some(conn) => {
                conn.dismantle();
            }
            None => {
                tracing::warn!("received broken event from unreferenced connection");
            }
        }
        Ok(())
    }

    fn on_dismantled(&mut self) -> Result<(), Error> {
        tracing::info!("connection closed");
        self.session_connected = false;
        self.connection = None;
        _ = self.cancel_channel.0.send(Cancel::Connection).map_err(|e| {
            tracing::error!(%e, "failed to send cancel to connection");
        });
        if let Some(sender) = self.shutdown_sender.as_ref() {
            tracing::debug!("shutting down after disconnecting");
            _ = sender.send(());
            Ok(())
        } else {
            self.act_on_target()
        }
    }

    fn on_info(&mut self, info: info::Info) -> Result<(), Error> {
        tracing::info!("on info: {info}");
        Ok(())
    }

    fn on_balance(&mut self, balance: balance::Balance) -> Result<(), Error> {
        tracing::info!("on balance: {balance}");
        Ok(())
    }

    fn on_inoperable_node(&mut self) -> Result<(), Error> {
        tracing::error!("node is inoperable - please check your configuration and network connectivity");
        _ = self.cancel_channel.0.send(Cancel::Node).map_err(|e| {
            tracing::error!(%e, "failed to send cancel to node");
        });
        let node = setup_node(
            self.config.entry_node(),
            self.sender.clone(),
            self.cancel_channel.1.clone(),
        );
        node.run();
        self.node = node;
        Ok(())
    }
}

fn read_config(config_path: &Path) -> Result<Config, Error> {
    let config = config::read(config_path)?;

    // print destinations warning
    if config.destinations().is_empty() {
        log_output::print_no_destinations();
    }

    Ok(config)
}

fn setup_node(
    entry_node: EntryNode,
    sender: crossbeam_channel::Sender<Event>,
    cancel_receiver: crossbeam_channel::Receiver<Cancel>,
) -> Node {
    let (s, r) = crossbeam_channel::unbounded();
    let node = Node::new(entry_node, s);
    thread::spawn(move || {
        loop {
            crossbeam_channel::select! {
                recv(cancel_receiver) -> msg => {
                    match msg {
                        Ok(Cancel::Node) => {
                            tracing::info!("shutting down node event handler");
                            break;
                        }
                        Ok(Cancel::Connection) => {
                            // ignoring connection cancel in node handler
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to receive cancel event");
                        }
                    }
                },
                recv(r) -> node_event => {
                    match node_event {
                        Ok(ref event) => {
                                _ = sender.send(Event::NodeEvent(event.clone())).map_err(|error| {
                                    tracing::error!(%event, %error, "failed to send NodeEvent event");
                                });
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to receive event");
                        }
                    }
                }
            }
        }
    });
    node
}
