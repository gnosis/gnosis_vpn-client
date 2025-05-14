use std::fmt::{self, Display};
use std::thread;
use std::time::{Duration, SystemTime};

use crossbeam_channel;
use rand::Rng;
use reqwest::{blocking, StatusCode};
use thiserror::Error;

use crate::entry_node::EntryNode;
use crate::log_output;
use crate::remote_data;
use crate::session::{self, Session};
use crate::wg_client;

pub use destination::{Destination, SessionParameters};

pub mod destination;

#[derive(Clone, Debug)]
pub enum Event {
    Connected(ConnectInfo),
    Disconnected,
}

#[derive(Clone, Debug)]
pub struct ConnectInfo {
    pub endpoint: String,
    pub wg_registration: wg_client::Register,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("No active session")]
    NotConnected,
    #[error("Failed to send message")]
    ChannelError(#[from] crossbeam_channel::SendError<()>),
}

/// Represents the different phases of a connection
/// Up: Idle -> BridgeSessionOpen -> WgRegistrationReceived -> BridgeSessionClosed -> MainSessionOpen
/// Down: MainSessionOpen -> MainSessionClosed -> BridgeSessionOpen -> WgUnregistrationReceived -> BridgeSessionClosed -> Idle
#[derive(Clone, Debug)]
enum Phase {
    Idle,
    BridgeSessionOpen,
    BridgeSessionClosed,
    WgRegistrationReceived,
    WgUnregistrationReceived,
    MainSessionOpen,
    MainSessionClosed,
}

#[derive(Debug)]
enum InternalEvent {
    SetUpSession(Result<Session, session::Error>),
    TearDownSession(Result<(), session::Error>),
    RegisterWg(Result<wg_client::Register, wg_client::Error>),
    UnregisterWg(Result<(), wg_client::Error>),
    ListSessions(Result<Vec<Session>, session::Error>),
}

#[derive(Clone, Debug)]
pub struct Connection {
    // message passing helper
    abort_channel: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),
    establish_channel: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),
    dismantle_channel: (
        crossbeam_channel::Sender<RuntimeData>,
        crossbeam_channel::Receiver<RuntimeData>,
    ),

    // reuse http client
    client: blocking::Client,

    // dynamic runtime data
    runtime_data: RuntimeData,

    // static input data
    entry_node: EntryNode,
    destination: Destination,
    wg_public_key: String,
    sender: crossbeam_channel::Sender<Event>,
}

#[derive(Clone, Debug)]
struct RuntimeData {
    phase: Phase,
    session_since: Option<(Session, SystemTime)>,
    wg_registration: Option<wg_client::Register>,
}

#[derive(Debug, Error)]
enum InternalError {
    #[error("Missing session data")]
    SessionNotSet,
    #[error("Missing WireGuard registration data")]
    WgRegistrationNotSet,
    #[error("Invalid phase for action")]
    UnexpectedPhase,
}

impl Connection {
    pub fn new(
        entry_node: &EntryNode,
        destination: &Destination,
        wg_public_key: &str,
        sender: crossbeam_channel::Sender<Event>,
    ) -> Self {
        let runtime_data = RuntimeData {
            phase: Phase::Idle,
            session_since: None,
            wg_registration: None,
        };
        Connection {
            abort_channel: crossbeam_channel::bounded(1),
            establish_channel: crossbeam_channel::bounded(1),
            dismantle_channel: crossbeam_channel::bounded(1),
            client: blocking::Client::new(),
            destination: destination.clone(),
            entry_node: entry_node.clone(),
            runtime_data,
            sender: sender.clone(),
            wg_public_key: wg_public_key.to_string(),
        }
    }

    pub fn has_destination(&self, destination: &Destination) -> bool {
        self.destination == *destination
    }

    pub fn destination(&self) -> Destination {
        self.destination.clone()
    }

    pub fn establish(&mut self) {
        let mut me = self.clone();
        thread::spawn(move || loop {
            let result = me.act_up();
            let recv_event = match result {
                Ok(recv_event) => recv_event,
                Err(error) => {
                    tracing::error!(%error, "Critical error during connection establishment - halting");
                    crossbeam_channel::never()
                }
            };
            crossbeam_channel::select! {
                // waiting on dismantle signal for providing runtime data
                recv(me.establish_channel.1) -> res => {
                    match res {
                        Ok(()) => {
                            match me.dismantle_channel.0.send(me.runtime_data) {
                                Ok(()) => (),
                                Err(error) => {
                                    tracing::error!(%error, "Critical error sending runtime data on dismantle channel - halting");
                                }
                            }
                            break;
                        }
                        Err(error) => {
                            tracing::error!(%error, "Failed receiving signal on establish channel");
                        }
                    }
                },
                recv(me.abort_channel.1) -> res => {
                    match res {
                        Ok(()) => {
                            tracing::warn!("Received abort signal during connection establishment");
                            break;
                        }
                        Err(error) => {
                            tracing::error!(%error, "Failed receiving signal on abort channel during connection establishment");
                        }
                    }
                },
                recv(recv_event) -> res => {
                    match res {
                        Ok(evt) => {
                                tracing::info!(event = ?evt, "Received event");
                                me.act_event(evt);
                        }
                        Err(error) => {
                            tracing::error!(%error, "Failed receiving event");
                        }
                    }
                }
            }
        });
    }

    pub fn dismantle(&mut self) {
        let mut me = self.clone();
        thread::spawn(move || loop {
            match me.establish_channel.0.send(()) {
                Ok(()) => (),
                Err(error) => {
                    tracing::error!(%error, "Critical error sending dismantle signal on establish channel - halting");
                    break;
                }
            }
            me.runtime_data = crossbeam_channel::select! {
                recv(me.dismantle_channel.1) -> res => {
                    match res {
                        Ok(data) => data,
                        Err(error) => {
                            tracing::error!(%error, "Critical error receiving runtime data on dismantle channel - halting");
                            break;
                        }
                    }
                }
                default(Duration::from_secs(3)) => {
                            tracing::error!("Critical timeout receiving runtime data on dismantle channel - halting");
                            break;
                }
            };

            let result = me.act_down();
            let recv_event = match result {
                Ok(recv_event) => recv_event,
                Err(error) => {
                    tracing::error!(%error, "Critical error during connection dismantling - halting");
                    crossbeam_channel::never()
                }
            };
            crossbeam_channel::select! {
                recv(me.abort_channel.1) -> res => {
                    match res {
                        Ok(()) => {
                            tracing::warn!("Received abort signal during connection dismantling");
                            break;
                        }
                        Err(error) => {
                            tracing::error!(%error, "Failed receiving signal on abort channel during connection dismantling");
                        }
                    }
                }
                recv(recv_event) -> res => {
                    match res {
                        Ok(evt) => {
                                tracing::info!(event = ?evt, "Received event");
                                me.act_event(evt);
                        }
                        Err(error) => {
                            tracing::error!(%error, "Failed receiving event");
                        }
                    }
                }
            }
        });
    }

    pub fn abort(&self) {
        tracing::info!("Aborting connection");
        self.abort_channel.0.send(()).unwrap_or_else(|error| {
            tracing::error!(%error, "Failed sending signal on abort channel");
        });
    }

    fn act_up(&mut self) -> Result<crossbeam_channel::Receiver<InternalEvent>, InternalError> {
        tracing::info!(runtime_data = %self.runtime_data, "Establishing connection");
        match self.runtime_data.phase {
            Phase::Idle => self.idle2bridge(),
            Phase::BridgeSessionOpen => self.bridge2wg(),
            Phase::WgRegistrationReceived => self.wg2teardown(),
            Phase::BridgeSessionClosed => self.teardown2main(),
            Phase::MainSessionOpen | Phase::MonitorMainSession => self.monitor(),
            Phase::MainSessionClosed | Phase::WgUnregistrationReceived => Err(InternalError::UnexpectedPhase),
        }
    }

    fn act_down(&mut self) -> Result<crossbeam_channel::Receiver<InternalEvent>, InternalError> {
        tracing::info!(runtime_data = %self.runtime_data, "Dismantling connection");
        match self.runtime_data.phase {
            _ => Err(InternalError::UnexpectedPhase),
        }
    }

    fn act_event(&mut self, event: InternalEvent) {
        match event {
            InternalEvent::BridgeSession(res) => match res {
                Ok(session) => {
                    self.runtime_data.phase = Phase::BridgeSessionOpen;
                    self.runtime_data.session_since = Some((session, SystemTime::now()));
                }
                // Some(Object {"status": String("LISTEN_HOST_ALREADY_USED")}) })))
                Err(session::Error::RemoteData(remote_data::CustomError {
                    status: Some(StatusCode::CONFLICT),
                    value: Some(json),
                    reqw_err: _,
                })) => {
                    if json["status"] == "LISTEN_HOST_ALREADY_USED" {
                        // TODO hanlde dismantling on existing port
                    }
                    tracing::error!(?json, "Failed to open session - CONFLICT");
                }
                Err(error) => {
                    tracing::error!(%error, "Failed to open session");
                }
            },
            InternalEvent::RegisterWg(res) => match res {
                Ok(register) => {
                    self.runtime_data.phase = Phase::WgRegistrationReceived;
                    self.runtime_data.wg_registration = Some(register);
                }
                Err(error) => {
                    tracing::error!(%error, "Failed to register wg client");
                }
            },
            InternalEvent::TearDownBridgeSession(res) => match res {
                Ok(_) => {
                    self.runtime_data.session_since = None;
                }
                Err(error) => {
                    tracing::error!(%error, "Failed to tear down bridge session");
                    self.runtime_data.phase = Phase::WgRegistrationReceived;
                }
            },
            InternalEvent::MainSession(res) => match res {
                Ok(session) => match self.runtime_data.wg_registration.as_ref() {
                    Some(wg_registration) => {
                        self.runtime_data.session_since = Some((session.clone(), SystemTime::now()));
                        let host = self
                            .entry_node
                            .endpoint
                            .host()
                            .map(|host| host.to_string())
                            .unwrap_or("".to_string());
                        _ = self.sender.send(Event::Connected(ConnectInfo {
                            endpoint: format!("{}:{}", host, session.port),
                            wg_registration: wg_registration.clone(),
                        }));
                    }
                    None => {
                        tracing::error!("No wg registration found, when it should have been set");
                    }
                },
                Err(error) => {
                    tracing::error!(%error, "Failed to open main session");
                }
            },
            InternalEvent::ListSessions(res) => match res {
                Ok(sessions) => match self.runtime_data.session_since.as_ref() {
                    Some((session, since)) => {
                        if session.verify_open(&sessions) {
                            tracing::info!(?sessions, "session verified open since {}", log_output::elapsed(&since));
                        } else {
                            tracing::info!("Session is closed");
                            self.runtime_data.session_since = None;
                            self.runtime_data.phase = Phase::BridgeSessionClosed;
                            _ = self.sender.send(Event::Disconnected)
                        }
                    }
                    None => {
                        tracing::warn!("List session results received but no session to verify");
                    }
                },
                Err(error) => {
                    tracing::error!(%error, "Failed to list sessions");
                }
            },
        }
    }

    /// Transition from Idle to BridgeSessionOpen
    fn idle2bridge(&mut self) -> Result<crossbeam_channel::Receiver<InternalEvent>, InternalError> {
        let params = session::OpenSession::bridge(
            &self.entry_node,
            &self.destination.peer_id,
            &self.destination.bridge.capabilities,
            &self.destination.path,
            &self.destination.bridge.target,
            &Duration::from_secs(15),
        );
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = Session::open(&client, &params);
            _ = s.send(InternalEvent::BridgeSession(res));
        });
        Ok(r)
    }

    /// Transition from BridgeSessionOpen to WgRegistrationReceived
    fn bridge2wg(&mut self) -> Result<crossbeam_channel::Receiver<InternalEvent>, InternalError> {
        let (session, _) = self
            .runtime_data
            .session_since
            .as_ref()
            .ok_or(InternalError::SessionNotSet)?;
        let ri = wg_client::RegisterInput::new(&self.wg_public_key, &self.entry_node.endpoint, &session);
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = wg_client::register(&client, &ri);
            _ = s.send(InternalEvent::RegisterWg(res));
        });
        Ok(r)
    }

    /// Transition from WgRegistrationReceived to BridgeSessionClosed
    fn wg2teardown(&mut self) -> Result<crossbeam_channel::Receiver<InternalEvent>, InternalError> {
        let (session, _) = self
            .runtime_data
            .session_since
            .as_ref()
            .ok_or(InternalError::SessionNotSet)?
            .clone();
        self.runtime_data
            .wg_registration
            .as_ref()
            .ok_or(InternalError::WgRegistrationNotSet)?;
        self.runtime_data.phase = Phase::BridgeSessionClosed;
        let params = session::CloseSession::new(&self.entry_node, &Duration::from_secs(15));
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = session.close(&client, &params);
            _ = s.send(InternalEvent::TearDownBridgeSession(res));
        });
        Ok(r)
    }

    /// Transition from BridgeSessionClosed to MainSessionOpen
    fn teardown2main(&mut self) -> Result<crossbeam_channel::Receiver<InternalEvent>, InternalError> {
        self.runtime_data.phase = Phase::MainSessionOpen;
        let params = session::OpenSession::main(
            &self.entry_node,
            &self.destination.peer_id,
            &self.destination.wg.capabilities,
            &self.destination.path,
            &self.destination.wg.target,
            &Duration::from_secs(20),
        );
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = Session::open(&client, &params);
            _ = s.send(InternalEvent::MainSession(res));
        });
        Ok(r)
    }

    fn monitor(&mut self) -> Result<crossbeam_channel::Receiver<InternalEvent>, InternalError> {
        let (session, _) = self
            .runtime_data
            .session_since
            .as_ref()
            .ok_or(InternalError::SessionNotSet)?;
        self.runtime_data.phase = Phase::MonitorMainSession;
        let params = session::ListSession::new(&self.entry_node, &session.protocol, &Duration::from_secs(30));
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let mut rng = rand::thread_rng();
            let delay = Duration::from_secs(rng.gen_range(5..10));
            let after = crossbeam_channel::after(delay);
            crossbeam_channel::select! {
                recv(after) -> _ => {
                    let res = Session::list(&client, &params);
                    _ = s.send(InternalEvent::ListSessions(res));
                }
            }
        });
        Ok(r)
    }
}

impl Display for RuntimeData {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let phase = match self.phase {
            Phase::Idle => "Idle",
            Phase::BridgeSessionOpen => "BridgeSessionOpen",
            Phase::BridgeSessionClosed => "BridgeSessionClosed",
            Phase::WgRegistrationReceived => "WgRegistrationReceived",
            Phase::WgUnregistrationReceived => "WgUnregistrationReceived",
            Phase::MainSessionOpen => "MainSessionOpen",
            Phase::MainSessionClosed => "MainSessionClosed",
        };
        write!(f, "RuntimeData[{}", phase);
        if let Some((_session, since)) = &self.session_since {
            write!(f, ", session since {}", log_output::elapsed(since));
        }
        if let Some(wg_registration) = &self.wg_registration {
            write!(f, ", {}", wg_registration);
        }
        write!(f, "]")
    }
}
