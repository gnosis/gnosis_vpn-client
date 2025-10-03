use edgli::EdgliProcesses;
use edgli::hopr_lib::Address;
use edgli::hopr_lib::exports::crypto::types::prelude::ChainKeypair;
use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use thiserror::Error;
use url::Url;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread;

use gnosis_vpn_lib::command::{self, Command, Response};
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::connection::{self, Connection, destination::Destination};
use gnosis_vpn_lib::hopr::{Hopr, HoprError, config as hopr_config, identity};
use gnosis_vpn_lib::node::{self, Node};
use gnosis_vpn_lib::onboarding::{self, Onboarding};
use gnosis_vpn_lib::{balance, info, wg_tooling};

use crate::event::Event;
use crate::hopr_params::{self, HoprParams};

#[derive(Debug, Error)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(#[from] config::Error),
    #[error("WireGuard error: {0}")]
    WgTooling(#[from] wg_tooling::Error),
    #[error("HOPR error: {0}")]
    Hopr(#[from] HoprError),
    #[error("Hopr config error: {0}")]
    HoprConfig(#[from] hopr_config::Error),
    #[error("Hopr identity error: {0}")]
    HoprIdentity(#[from] identity::Error),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Balance error: {0}")]
    Balance(#[from] balance::Error),
    #[error("Edge client not ready")]
    EdgeNotReady,
}

pub struct Core {
    // configuration data
    config: Config,
    // global event transmitter
    sender: crossbeam_channel::Sender<Event>,
    // shutdown event emitter
    shutdown_sender: Option<crossbeam_channel::Sender<()>>,
    // internal cancellation sender
    cancel_channel: (crossbeam_channel::Sender<Cancel>, crossbeam_channel::Receiver<Cancel>),

    // depending on safe creation state
    run_mode: RunMode,

    // connection to exit node
    connection: Option<connection::Connection>,
    session_connected: bool,
    target_destination: Option<Destination>,

    // provided hopr params
    hopr_params: HoprParams,

    // results from node
    balance: Option<balance::Balances>,
    info: Option<info::Info>,

    // presafe results
    presafe_balance: Option<balance::PreSafe>,
}

enum RunMode {
    #[allow(dead_code)]
    PreSafe(Box<Onboarding>),
    Full {
        // hoprd edge client
        hopr: Arc<Hopr>,
        // thread loop around funding and general node
        #[allow(dead_code)]
        node: Box<Node>,
    },
}

enum Cancel {
    Node,
    Connection,
    Onboarding,
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
        let run_mode = determine_run_mode(
            sender.clone(),
            cancel_channel.1.clone(),
            &hopr_params,
            config.channel_targets(),
        )?;

        Ok(Core {
            config,
            sender,
            shutdown_sender: None,
            connection: None,
            run_mode,
            session_connected: false,
            target_destination: None,
            balance: None,
            info: None,
            hopr_params,
            cancel_channel,
            presafe_balance: None,
        })
    }

    pub fn shutdown(&mut self) -> crossbeam_channel::Receiver<()> {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        self.shutdown_sender = Some(sender);
        match &self.run_mode {
            RunMode::PreSafe(_) => {
                _ = self.cancel_channel.0.send(Cancel::Onboarding).map_err(|e| {
                    tracing::error!(%e, "failed to send cancel to onboarding");
                });
                tracing::debug!("shutting down from presafe mode");
                if let Some(s) = self.shutdown_sender.as_ref() {
                    _ = s.send(());
                };
            }
            RunMode::Full { .. } => {
                _ = self.cancel_channel.0.send(Cancel::Onboarding).map_err(|e| {
                    tracing::error!(%e, "failed to send cancel to onboarding");
                });
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
                tracing::debug!("gathering status");
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
                tracing::debug!(?status, ?self.balance, "gathering status foo");

                let funding_issues: Option<Vec<balance::FundingIssue>> = match &self.balance {
                    Some(balance) => Some(balance.to_funding_issues(self.config.channel_targets().len())?),
                    None => None,
                };

                tracing::debug!(?funding_issues, "FundingIssue");

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
                    let issues: Vec<balance::FundingIssue> =
                        balance.to_funding_issues(self.config.channel_targets().len())?;
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
            Command::RefreshNode => match &self.run_mode {
                RunMode::PreSafe(_) => {
                    tracing::info!("edge client not running - cannot refresh node");
                    return Err(Error::EdgeNotReady);
                }
                RunMode::Full { .. } => {
                    self.run_mode = determine_run_mode(
                        self.sender.clone(),
                        self.cancel_channel.1.clone(),
                        &self.hopr_params,
                        self.config.channel_targets(),
                    )?;
                    Ok(Response::RefreshNode)
                }
            },
        }
    }

    pub fn handle_event(&mut self, event: Event) -> Result<(), Error> {
        tracing::debug!(%event, "handling event");
        match event {
            Event::Connection(connection::Event::Connected) => self.on_connected(),
            Event::Connection(connection::Event::Disconnected) => self.on_disconnected(),
            Event::Connection(connection::Event::Broken) => self.on_broken(),
            Event::Connection(connection::Event::Dismantled) => self.on_dismantled(),
            Event::Node(node::Event::Info(info)) => self.on_info(info),
            Event::Node(node::Event::Balance(balance)) => self.on_balance(balance),
            Event::Node(node::Event::BackoffExhausted) => self.on_inoperable_node(),
            Event::Onboarding(onboarding::Event::Balance(balance)) => self.on_onboarding_balance(balance),
            Event::Onboarding(onboarding::Event::SafeModule(safe_module)) => {
                self.on_onboarding_safe_module(safe_module)
            }
            Event::Onboarding(onboarding::Event::BackoffExhausted) => self.on_failed_onboarding(),
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

        // recheck run mode
        self.run_mode = determine_run_mode(
            self.sender.clone(),
            self.cancel_channel.1.clone(),
            &self.hopr_params,
            self.config.channel_targets(),
        )?;
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
                self.check_connect(&dest)
            }
            (None, None) => Ok(()),
        }
    }

    fn check_connect(&mut self, destination: &Destination) -> Result<(), Error> {
        match &self.run_mode {
            RunMode::PreSafe(_) => {
                tracing::error!("edge client not running - cannot connect");
                Err(Error::EdgeNotReady)
            }
            RunMode::Full { hopr, .. } => self.connect(destination, hopr.clone()),
        }
    }

    fn connect(&mut self, destination: &Destination, hopr: Arc<Hopr>) -> Result<(), Error> {
        let (s, r) = crossbeam_channel::unbounded();
        let config_wireguard = self.config.wireguard.clone();
        let wg = wg_tooling::WireGuard::from_config(config_wireguard)?;
        let config_connection = self.config.connection.clone();
        let mut conn = Connection::new(hopr.clone(), destination.clone(), wg, s, config_connection);
        conn.establish();
        self.connection = Some(conn);
        let sender = self.sender.clone();
        thread::spawn(move || {
            loop {
                crossbeam_channel::select! {
                    recv(r) -> conn_event => {
                        match conn_event {
                            Ok(event) => {
                                _ = sender.send(Event::Connection(event)).map_err(|error| {
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
        if let RunMode::Full { .. } = &self.run_mode {
            _ = self.cancel_channel.0.send(Cancel::Connection).map_err(|e| {
                tracing::error!(%e, "failed to send cancel to connection");
            });
        }
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
        self.run_mode = determine_run_mode(
            self.sender.clone(),
            self.cancel_channel.1.clone(),
            &self.hopr_params,
            self.config.channel_targets(),
        )?;
        Ok(())
    }

    fn on_onboarding_balance(&mut self, presafe: balance::PreSafe) -> Result<(), Error> {
        tracing::info!("on presafe balance: {presafe}");
        self.presafe_balance = Some(presafe);
        Ok(())
    }

    fn on_onboarding_safe_module(&mut self, safe_module: hopr_config::SafeModule) -> Result<(), Error> {
        tracing::info!(?safe_module, "on safe module - generating hoprd configuration");
        _ = self.cancel_channel.0.send(Cancel::Onboarding).map_err(|e| {
            tracing::error!(%e, "failed to send cancel to finished onboarding");
        });
        match self.hopr_params.config_mode.clone() {
            hopr_params::ConfigMode::Manual(_) => {
                tracing::warn!("manual configuration mode - not overwriting existing configuration");
                return Ok(());
            }
            hopr_params::ConfigMode::Generated { rpc_provider, network } => {
                let cfg = hopr_config::generate(network, rpc_provider, safe_module)?;
                hopr_config::write_default(&cfg)?;
            }
        };
        self.run_mode = determine_run_mode(
            self.sender.clone(),
            self.cancel_channel.1.clone(),
            &self.hopr_params,
            self.config.channel_targets(),
        )?;
        Ok(())
    }

    fn on_failed_onboarding(&mut self) -> Result<(), Error> {
        tracing::error!("onboarding failed - please check your configuration and network connectivity");
        self.run_mode = determine_run_mode(
            self.sender.clone(),
            self.cancel_channel.1.clone(),
            &self.hopr_params,
            self.config.channel_targets(),
        )?;
        Ok(())
    }
}

fn setup_onboarding(
    sender: crossbeam_channel::Sender<Event>,
    cancel_receiver: crossbeam_channel::Receiver<Cancel>,
    private_key: ChainKeypair,
    rpc_provider: Url,
    node_address: Address,
) -> Onboarding {
    let (s, r) = crossbeam_channel::unbounded();
    let onboarding = Onboarding::new(s, private_key, rpc_provider, node_address);
    thread::spawn(move || {
        loop {
            crossbeam_channel::select! {
                recv(cancel_receiver) -> msg => {
                    match msg {
                        Ok(Cancel::Onboarding) => {
                            tracing::info!("shutting down onboarding event handler");
                            break;
                        }
                        Ok(_) => {
                            // ignoring cancel event in onboarding handler
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "onboarding failed to receive cancel event");
                        }
                    }
                },
                recv(r) -> onboarding_event => {
                    match onboarding_event {
                        Ok(ref event) => {
                                _ = sender.send(Event::Onboarding(event.clone())).map_err(|error| {
                                    tracing::error!(%event, %error, "failed to send onboarding event");
                                });
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to receive event");
                        }
                    }
                },
            }
        }
    });
    onboarding
}

fn setup_node(
    sender: crossbeam_channel::Sender<Event>,
    cancel_receiver: crossbeam_channel::Receiver<Cancel>,
    edgli: Arc<Hopr>,
    hopr_startup_notifier: crossbeam_channel::Receiver<
        std::result::Result<Vec<EdgliProcesses>, edgli::hopr_lib::errors::HoprLibError>,
    >,
    channel_targets: Vec<Address>,
) -> Result<Node, Error> {
    let (s, r) = crossbeam_channel::unbounded();
    // gather channel relays

    let node = Node::new(s, edgli, channel_targets);
    thread::spawn(move || {
        let mut hopr_processes = Vec::new();

        loop {
            crossbeam_channel::select! {
                recv(cancel_receiver) -> msg => {
                    match msg {
                        Ok(Cancel::Node) => {
                            tracing::info!("shutting down node event handler");
                            break;
                        }
                        Ok(_) => {
                            // ignoring cancel event in node handler
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "node init failed to receive cancel event");
                        }
                    }
                },
                recv(hopr_startup_notifier) -> startup_result => {
                    match startup_result {
                        Ok(Ok(processes)) => {
                            tracing::info!("HOPR processes started: {:?}", processes);
                            hopr_processes = processes;
                            break
                        }
                        Ok(Err(e)) => {
                            tracing::error!(%e, "failed to start HOPR processes");
                            return;
                        }
                        Err(e) => {
                            tracing::error!(%e, "failed to receive HOPR startup notification");
                            return;
                        }
                    }
                }
            }
        }

        loop {
            crossbeam_channel::select! {
                recv(cancel_receiver) -> msg => {
                    match msg {
                        Ok(Cancel::Node) => {
                            tracing::info!("shutting down node event handler");
                            for process in &mut hopr_processes {
                                tracing::info!("shutting down HOPR process: {process}");
                                match process {
                                    EdgliProcesses::HoprLib(_process, handle) => handle.abort(),
                                    EdgliProcesses::Hopr(handle) => handle.abort(),
                                }
                            }
                            break;
                        }
                        Ok(_) => {
                            // ignoring cancel event in node handler
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "node loop failed to receive cancel event");
                        }
                    }
                },
                recv(r) -> node_event => {
                    match node_event {
                        Ok(ref event) => {
                                _ = sender.send(Event::Node(event.clone())).map_err(|error| {
                                    tracing::error!(%event, %error, "failed to send NodeEvent event");
                                });
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to receive event");
                        }
                    }
                },
            }
        }
    });
    Ok(node)
}

fn determine_run_mode(
    sender: crossbeam_channel::Sender<Event>,
    cancel_receiver: crossbeam_channel::Receiver<Cancel>,
    hopr_params: &HoprParams,
    channel_targets: Vec<Address>,
) -> Result<RunMode, Error> {
    let identity_file = match &hopr_params.identity_file {
        Some(path) => path.to_path_buf(),
        None => {
            let path = identity::file()?;
            tracing::info!(?path, "No HOPR identity file path provided - using default");
            path
        }
    };

    let identity_pass = match &hopr_params.identity_pass {
        Some(pass) => pass.to_string(),
        None => {
            let path = identity::pass_file()?;
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
                    let pw = identity::generate_pass();
                    fs::write(&path, pw.as_bytes())?;
                    Ok(pw)
                }
                Err(e) => Err(e),
            }?
        }
    };

    let cfg = match hopr_params.config_mode.clone() {
        hopr_params::ConfigMode::Manual(path) => hopr_config::from_path(path.as_ref())?,
        hopr_params::ConfigMode::Generated { rpc_provider, .. } => {
            let conf_file = hopr_config::config_file()?;
            if conf_file.exists() {
                hopr_config::from_path(&conf_file)?
            } else {
                let keys = identity::from_path(identity_file.as_path(), identity_pass)?;
                let node_address = keys.chain_key.public().to_address();
                let onboarding = setup_onboarding(
                    sender.clone(),
                    cancel_receiver.clone(),
                    keys.chain_key,
                    rpc_provider,
                    node_address,
                );
                onboarding.run();
                return Ok(RunMode::PreSafe(Box::new(onboarding)));
            }
        }
    };

    let (hopr_startup_notifier_tx, hopr_startup_notifier_rx) = crossbeam_channel::bounded(1);
    let keys = identity::from_path(identity_file.as_path(), identity_pass)?;
    let hoprd = Hopr::new(cfg, keys, hopr_startup_notifier_tx)?;
    let hopr = Arc::new(hoprd);

    let node = setup_node(
        sender.clone(),
        cancel_receiver.clone(),
        hopr.clone(),
        hopr_startup_notifier_rx.clone(),
        channel_targets,
    )?;

    node.run();

    Ok(RunMode::Full {
        hopr,
        node: Box::new(node),
    })
}
