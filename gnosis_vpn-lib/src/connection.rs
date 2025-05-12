use crossbeam_channel;
use rand::Rng;
use reqwest::{blocking, StatusCode};
use std::thread;
use std::time::{Duration, SystemTime};
use thiserror::Error;

use crate::entry_node::EntryNode;
use crate::log_output;
use crate::peer_id::PeerId;
use crate::remote_data;
use crate::session::{self, Session};
use crate::wg_client;

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

#[derive(Debug)]
pub enum Error {
    NotConnected,
}

/// Represents the different phases of a connection
/// Up: Idle -> SetUpBridgeSession -> RegisterWg -> TearDownBridgeSession -> SetUpMainSession -> MonitorMainSession
/// Down: MonitorMainSession -> TearDownMainSession -> SetUpBridgeSession -> UnregisterWg -> TearDownBridgeSession -> Idle
#[derive(Clone, Debug)]
enum Phase {
    Idle,
    SetUpBridgeSession,
    TearDownBridgeSession,
    RegisterWg,
    UnregisterWg,
    SetUpMainSession,
    TearDownMainSession,
    MonitorMainSession,
}

#[derive(Clone, Debug)]
enum Direction {
    Up,
    Down,
    Halt,
}

#[derive(Debug)]
enum InternalEvent {
    BridgeSession(Result<Session, session::Error>),
    RegisterWg(Result<wg_client::Register, wg_client::Error>),
    TearDownBridgeSession(Result<(), session::Error>),
    MainSession(Result<Session, session::Error>),
    ListSessions(Result<Vec<Session>, session::Error>),
}

#[derive(Clone, Debug)]
pub struct Destination {
    peer_id: PeerId,
    path: session::Path,
    bridge: SessionParameters,
    wg: SessionParameters,
}

#[derive(Clone, Debug)]
pub struct SessionParameters {
    target: session::Target,
    capabilities: Vec<session::Capability>,
}

#[derive(Clone, Debug)]
pub struct Connection {
    phase: Phase,
    direction: Direction,
    // runtime data
    abort_sender: Option<crossbeam_channel::Sender<()>>,
    client: blocking::Client,
    session_since: Option<(Session, SystemTime)>,
    wg_registration: Option<wg_client::Register>,
    // input data
    entry_node: EntryNode,
    destination: Destination,
    wg_public_key: String,
    sender: crossbeam_channel::Sender<Event>,
}

#[derive(Debug, Error)]
enum InternalError {
    #[error("Missing session data")]
    SessionNotSet,
    #[error("Missing WireGuard registration data")]
    WgRegistrationNotSet,
}

impl SessionParameters {
    pub fn new(target: &session::Target, capabilities: &Vec<session::Capability>) -> Self {
        Self {
            target: target.clone(),
            capabilities: capabilities.clone(),
        }
    }
}

impl Destination {
    pub fn new(peer_id: &PeerId, path: &session::Path, bridge: &SessionParameters, wg: &SessionParameters) -> Self {
        Self {
            peer_id: peer_id.clone(),
            path: path.clone(),
            bridge: bridge.clone(),
            wg: wg.clone(),
        }
    }
}

impl Connection {
    pub fn new(
        entry_node: &EntryNode,
        destination: &Destination,
        wg_public_key: &str,
        sender: crossbeam_channel::Sender<Event>,
    ) -> Self {
        Connection {
            abort_sender: None,
            client: blocking::Client::new(),
            destination: destination.clone(),
            direction: Direction::Halt,
            entry_node: entry_node.clone(),
            phase: Phase::Idle,
            sender: sender.clone(),
            session_since: None,
            wg_public_key: wg_public_key.to_string(),
            wg_registration: None,
        }
    }

    pub fn establish(&mut self) {
        let (send_abort, recv_abort) = crossbeam_channel::bounded(1);
        self.abort_sender = Some(send_abort);
        let mut me = self.clone();
        thread::spawn(move || loop {
            let result = me.act_up();
            let recv_event = match result {
                Ok(recv_event) => recv_event,
                Err(error) => {
                    tracing::error!(%error, "Failed to act up");
                    crossbeam_channel::never()
                }
            };
            crossbeam_channel::select! {
                recv(recv_abort) -> res => {
                    match res {
                        Ok(_) => {
                            me.act_abort();
                            break;
                        }
                        Err(error) => {
                            tracing::error!(%error, "Failed receiving abort signal");
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

    pub fn abort(&self) -> Result<(), crossbeam_channel::SendError<()>> {
        match &self.abort_sender {
            Some(sender) => {
                tracing::info!("Aborting connection");
                sender.send(())
            }
            None => {
                tracing::info!("Connection not started - nothing to abort");
                Ok(())
            }
        }
    }

    pub fn port(&self) -> Result<u16, Error> {
        match self.session_since.as_ref() {
            Some((session, _)) => Ok(session.port),
            None => Err(Error::NotConnected),
        }
    }

    fn act_up(&mut self) -> Result<crossbeam_channel::Receiver<InternalEvent>, InternalError> {
        self.direction = Direction::Up;
        tracing::info!(phase = ?self.phase, "Acting up");
        match self.phase {
            Phase::Idle => self.idle2bridge(),
            Phase::SetUpBridgeSession => self.bridge2wg(),
            Phase::RegisterWg => self.wg2teardown(),
            Phase::TearDownBridgeSession => self.teardown2main(),
            Phase::SetUpMainSession | Phase::MonitorMainSession => self.monitor(),
            Phase::TearDownMainSession | Phase::UnregisterWg => {
                panic!("Invalid phase for up action");
            }
        }
    }

    fn act_down(&self) {
        match self.phase {
            Phase::Idle => {}
            _ => {
                panic!("Invalid phase for down action");
            }
        }
    }

    fn act_event(&mut self, event: InternalEvent) {
        match event {
            InternalEvent::BridgeSession(res) => match res {
                Ok(session) => {
                    self.session_since = Some((session, SystemTime::now()));
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
                    self.phase = Phase::Idle;
                }
                Err(error) => {
                    tracing::error!(%error, "Failed to open session");
                    self.phase = Phase::Idle;
                }
            },
            InternalEvent::RegisterWg(res) => match res {
                Ok(register) => {
                    self.wg_registration = Some(register);
                }
                Err(error) => {
                    tracing::error!(%error, "Failed to register wg client");
                    self.phase = Phase::SetUpBridgeSession;
                }
            },
            InternalEvent::TearDownBridgeSession(res) => match res {
                Ok(_) => {
                    self.session_since = None;
                }
                Err(error) => {
                    tracing::error!(%error, "Failed to tear down bridge session");
                    self.phase = Phase::RegisterWg;
                }
            },
            InternalEvent::MainSession(res) => match res {
                Ok(session) => match self.wg_registration.as_ref() {
                    Some(wg_registration) => {
                        self.session_since = Some((session.clone(), SystemTime::now()));
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
                Ok(sessions) => match self.session_since.as_ref() {
                    Some((session, since)) => {
                        if session.verify_open(&sessions) {
                            tracing::info!(?sessions, "session verified open since {}", log_output::elapsed(&since));
                        } else {
                            tracing::info!("Session is closed");
                            self.session_since = None;
                            self.phase = Phase::TearDownBridgeSession;
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

    fn act_abort(&self) {
        match self.phase {
            _ => {
                panic!("Invalid phase for abort action");
            }
        }
    }

    /// Transition from Idle to SetUpBridgeSession
    fn idle2bridge(&mut self) -> Result<crossbeam_channel::Receiver<InternalEvent>, InternalError> {
        self.phase = Phase::SetUpBridgeSession;
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

    /// Transition from SetUpBridgeSession to RegisterWg
    fn bridge2wg(&mut self) -> Result<crossbeam_channel::Receiver<InternalEvent>, InternalError> {
        let (session, _) = self.session_since.as_ref().ok_or(InternalError::SessionNotSet)?;
        self.phase = Phase::RegisterWg;
        let ri = wg_client::RegisterInput::new(&self.wg_public_key, &self.entry_node.endpoint, &session);
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = wg_client::register(&client, &ri);
            _ = s.send(InternalEvent::RegisterWg(res));
        });
        Ok(r)
    }

    /// Transition from RegisterWg to TearDownBridgeSession
    fn wg2teardown(&mut self) -> Result<crossbeam_channel::Receiver<InternalEvent>, InternalError> {
        let (session, _) = self.session_since.as_ref().ok_or(InternalError::SessionNotSet)?.clone();
        self.wg_registration
            .as_ref()
            .ok_or(InternalError::WgRegistrationNotSet)?;
        self.phase = Phase::TearDownBridgeSession;
        let params = session::CloseSession::new(&self.entry_node, &Duration::from_secs(15));
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = session.close(&client, &params);
            _ = s.send(InternalEvent::TearDownBridgeSession(res));
        });
        Ok(r)
    }

    /// Transition from TearDownBridgeSession to SetUpMainSession
    fn teardown2main(&mut self) -> Result<crossbeam_channel::Receiver<InternalEvent>, InternalError> {
        self.phase = Phase::SetUpMainSession;
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
        let (session, _) = self.session_since.as_ref().ok_or(InternalError::SessionNotSet)?;
        self.phase = Phase::MonitorMainSession;
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
