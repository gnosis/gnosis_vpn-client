use thiserror::Error;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use gnosis_vpn_lib::command::{self, Command, Response};
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::connection::{self, Connection, destination::Destination};
use gnosis_vpn_lib::hopr::{Hopr, HoprError, config as HoprConfig, identity as HoprIdentity};
use gnosis_vpn_lib::node::{self, Node};
use gnosis_vpn_lib::{balance, info, wg_tooling};

use crate::event::Event;

pub struct Core {
    // configuration data
    config: Config,
    // global event transmitter
    sender: crossbeam_channel::Sender<Event>,
    // shutdown event emitter
    shutdown_sender: Option<crossbeam_channel::Sender<()>>,

    // hopr edge node
    edgli: Arc<Hopr>,

    // connection to exit node
    connection: Option<connection::Connection>,
    // entry node
    node: Node,
    session_connected: bool,
    target_destination: Option<Destination>,

    // internal cancellation sender
    cancel_channel: (crossbeam_channel::Sender<Cancel>, crossbeam_channel::Receiver<Cancel>),

    // results from node
    balance: Option<balance::Balances>,
    info: Option<info::Info>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(#[from] config::Error),
    #[error("WireGuard error: {0}")]
    WgTooling(#[from] wg_tooling::Error),
    #[error("HOPR error: {0}")]
    Hopr(#[from] HoprError),
    #[error("Hopr config error: {0}")]
    HoprConfig(#[from] HoprConfig::Error),
    #[error("Hopr identity error: {0}")]
    HoprIdentity(#[from] HoprIdentity::Error),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
}

pub struct HoprParams {
    pub config_path: PathBuf,
    pub identity_file: Option<PathBuf>,
    pub identity_pass: Option<String>,
}

enum Cancel {
    Node,
    Connection,
}

impl Core {
    pub fn init(
        config_path: &Path,
        sender: crossbeam_channel::Sender<Event>,
        hopr_params: HoprParams,
    ) -> Result<Core, Error> {
        let config = config::read(config_path)?;
        wg_tooling::available()?;

        let cancel_channel = crossbeam_channel::unbounded::<Cancel>();
        let hopr = init_hopr(&hopr_params)?;
        let edgli = Arc::new(hopr);
        let node = setup_node(edgli.clone(), sender.clone(), cancel_channel.1.clone());

        Ok(Core {
            config,
            edgli,
            sender,
            shutdown_sender: None,
            connection: None,
            node,
            session_connected: false,
            target_destination: None,
            balance: None,
            info: None,
            cancel_channel,
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
        let destinations = self.config.destinations.clone();
        match cmd {
            Command::Ping => Ok(Response::Pong),
            Command::Connect(address) => match destinations.get(address) {
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

                let funding_issues: Option<Vec<balance::FundingIssue>> = self.balance.as_ref().map(|b| b.into());
                Ok(Response::status(command::StatusResponse::new(
                    status,
                    self.config
                        .destinations
                        .values()
                        .map(|v| {
                            let dest = v.clone();
                            dest.into()
                        })
                        .collect(),
                    funding_issues.into(),
                    self.info.as_ref().map(|i| i.network.to_string()),
                )))
            }
            Command::Balance => match (&self.balance, &self.info) {
                (Some(balance), Some(info)) => {
                    let issues: Vec<balance::FundingIssue> = balance.into();
                    let resp = command::BalanceResponse::new(
                        format!("{} xDai", balance.node_xdai),
                        format!("{} wxHOPR", balance.safe_wxhopr),
                        format!("{} wxHOPR", balance.channels_out_wxhopr),
                        issues,
                        command::Addresses {
                            node: info.node_address,
                            safe: info.safe_address,
                        },
                    );
                    Ok(Response::Balance(Some(resp)))
                }
                _ => Ok(Response::Balance(None)),
            },
            Command::RefreshNode => {
                self.refresh_node()?;
                Ok(Response::RefreshNode)
            }
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
        let config = config::read(config_path)?;
        // handle existing connection
        if let Some(conn) = &mut self.connection {
            tracing::info!(current = %conn.destination(), "disconnecting from current destination due to configuration update");
            conn.dismantle();
        }
        // update target
        if let Some(dest) = self.target_destination.as_ref() {
            if let Some(new_dest) = config.destinations.get(&dest.address) {
                tracing::debug!(current = %dest, new = %new_dest, "target destination updated");
                self.target_destination = Some(new_dest.clone());
            } else {
                tracing::info!(current = %dest, "clearing target destination - not found in new configuration");
                self.target_destination = None;
            }
        }

        // handle existing node
        self.balance = None;
        self.info = None;
        self.config = config;

        // setup new node
        self.refresh_node()
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
        let config_wireguard = self.config.wireguard.clone();
        let wg = wg_tooling::WireGuard::from_config(config_wireguard)?;
        let config_connection = self.config.connection.clone();
        let mut conn = Connection::new(self.edgli.clone(), destination.clone(), wg, s, config_connection);
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
        self.info = Some(info);
        Ok(())
    }

    fn on_balance(&mut self, balance: balance::Balances) -> Result<(), Error> {
        tracing::info!("on balance: {balance}");
        self.balance = Some(balance);
        Ok(())
    }

    fn on_inoperable_node(&mut self) -> Result<(), Error> {
        tracing::error!("node is inoperable - please check your configuration and network connectivity");
        self.refresh_node()
    }

    fn refresh_node(&mut self) -> Result<(), Error> {
        self.node.cancel();
        _ = self.cancel_channel.0.send(Cancel::Node).map_err(|e| {
            tracing::error!(%e, "failed to send cancel to node");
        });
        let node = setup_node(self.edgli.clone(), self.sender.clone(), self.cancel_channel.1.clone());
        node.run();
        self.node = node;
        Ok(())
    }
}

fn setup_node(
    edgli: Arc<Hopr>,
    sender: crossbeam_channel::Sender<Event>,
    cancel_receiver: crossbeam_channel::Receiver<Cancel>,
) -> Node {
    let (s, r) = crossbeam_channel::unbounded();
    let node = Node::new(edgli, s);
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

fn init_hopr(hopr_params: &HoprParams) -> Result<Hopr, Error> {
    let identity_file = match &hopr_params.identity_file {
        Some(path) => path.to_path_buf(),
        None => {
            let path = HoprIdentity::identity_file()?;
            tracing::info!(?path, "No HOPR identity file path provided - using default");
            path
        }
    };
    let identity_pass = match &hopr_params.identity_pass {
        Some(pass) => pass.to_string(),
        None => {
            let path = HoprIdentity::identity_pass()?;
            match fs::read_to_string(&path) {
                Ok(p) => {
                    tracing::warn!(?path, "No HOPR identity pass provided - read from file instead");
                    Ok(p)
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    tracing::warn!(
                        ?path,
                        "No HOPR identity pass provided - generating new one and storing alongside identity file"
                    );
                    let pw = HoprIdentity::generate_pass();
                    fs::write(&path, pw.as_bytes())?;
                    Ok(pw)
                }
                Err(e) => Err(e),
            }?
        }
    };

    let keys = HoprIdentity::from_path(identity_file.as_path(), identity_pass)?;
    let cfg = HoprConfig::from_path(&hopr_params.config_path)?;
    let hopr = Hopr::new(cfg, keys)?;
    Ok(hopr)
}
