use backoff::{backoff::Backoff, ExponentialBackoff};
use std::fmt::{self, Display};
use std::thread;
use std::time::{Duration, SystemTime};

use crossbeam_channel;
use rand::Rng;
use reqwest::blocking;
use thiserror::Error;

use crate::entry_node::EntryNode;
use crate::log_output;
use crate::session::{self, Session};
use crate::wg_client::{self, Registration};

pub use destination::{Destination, SessionParameters};

pub mod destination;

#[derive(Clone, Debug)]
pub enum Event {
    Connected(ConnectInfo),
    Disconnected,
    Dismantled,
}

#[derive(Clone, Debug)]
pub struct ConnectInfo {
    pub endpoint: String,
    pub registration: Registration,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("No active session")]
    NotConnected,
    #[error("Failed to send message")]
    ChannelError(#[from] crossbeam_channel::SendError<()>),
}

/// Represents the different phases of establishing a connection.
/// Up: Idle -> BridgeSessionOpen -> WgRegistrationReceived -> BridgeSessionClosed -> MainSessionOpen
#[derive(Clone, Debug)]
enum PhaseUp {
    Idle,
    BridgeSessionOpen(Session),
    WgRegistrationReceived(Session, Registration),
    BridgeSessionClosed(Registration),
    MainSessionOpen(Session, SystemTime, Registration),
}

/// Represents the different phases of dismantling a connection.
/// Down: MainSessionOpen -> MainSessionClosed -> BridgeSessionOpen -> WgUnregistrationReceived -> BridgeSessionClosed -> Idle
#[derive(Clone, Debug)]
enum PhaseDown {
    Idle,
    MainSessionOpen(Session, SystemTime, Registration),
    MainSessionClosed(Registration),
    BridgeSessionOpen(Session, Registration),
    WgUnregistrationReceived(Session),
    BridgeSessionClosed,
}

#[derive(Debug)]
enum InternalEvent {
    SetUpBridgeSession(Result<Session, session::Error>),
    TearDownBridgeSession(Result<(), session::Error>),
    SetUpMainSession(Result<Session, session::Error>),
    TearDownMainSession(Result<(), session::Error>),
    RegisterWg(Result<Registration, wg_client::Error>),
    UnregisterWg(Result<(), wg_client::Error>),
    ListSessions(Result<Vec<Session>, session::Error>),
}

#[derive(Clone, Debug)]
enum BackoffState {
    Inactive,
    Active(ExponentialBackoff),
    Triggered(ExponentialBackoff),
}

#[derive(Clone, Debug)]
pub struct Connection {
    // message passing helper
    abort_channel: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),
    establish_channel: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),
    dismantle_channel: (crossbeam_channel::Sender<PhaseUp>, crossbeam_channel::Receiver<PhaseUp>),

    // reuse http client
    client: blocking::Client,

    // dynamic runtime data
    phase_up: PhaseUp,
    phase_down: PhaseDown,
    backoff: BackoffState,

    // static input data
    entry_node: EntryNode,
    destination: Destination,
    wg_public_key: String,
    sender: crossbeam_channel::Sender<Event>,
}

#[derive(Debug, Error)]
enum InternalError {
    #[error("Invalid phase for action")]
    UnexpectedPhase,
    #[error("Internal session error")]
    SessionError(#[from] session::Error),
    #[error("Internal WireGuard error")]
    WgError(#[from] wg_client::Error),
    #[error("Channel send error")]
    SendError(#[from] crossbeam_channel::SendError<Event>),
    #[error("Unexpected event")]
    UnexecptedEvent(InternalEvent),
}

impl Connection {
    pub fn new(
        entry_node: &EntryNode,
        destination: &Destination,
        wg_public_key: &str,
        sender: crossbeam_channel::Sender<Event>,
    ) -> Self {
        Connection {
            abort_channel: crossbeam_channel::bounded(1),
            backoff: BackoffState::Inactive,
            client: blocking::Client::new(),
            destination: destination.clone(),
            dismantle_channel: crossbeam_channel::bounded(1),
            entry_node: entry_node.clone(),
            establish_channel: crossbeam_channel::bounded(1),
            phase_down: PhaseDown::Idle,
            phase_up: PhaseUp::Idle,
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
            // handle backoff here
            // Inactive - no backoff was set, act up
            // Active - backoff was set and can trigger, don't act until backoff delay
            // Triggered - backoff was triggered, time to act up again keeping backoff active
            let (recv_event, recv_backoff) = match me.backoff {
                BackoffState::Inactive => (me.act_up(), crossbeam_channel::never()),
                BackoffState::Active(mut backoff) => match backoff.next_backoff() {
                    Some(delay) => {
                        me.backoff = BackoffState::Triggered(backoff);
                        (crossbeam_channel::never(), crossbeam_channel::after(delay))
                    }
                    None => {
                        tracing::error!("Critical error: backoff exhausted - halting");
                        break;
                    }
                },
                BackoffState::Triggered(backoff) => {
                    me.backoff = BackoffState::Active(backoff);
                    (me.act_up(), crossbeam_channel::never())
                }
            };
            crossbeam_channel::select! {
                // waiting on dismantle signal for providing runtime data
                recv(me.establish_channel.1) -> res => {
                    match res {
                        Ok(()) => {
                            match me.dismantle_channel.0.send(me.phase_up) {
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
                recv(recv_backoff) -> _ => {
                    tracing::debug!("Backoff delay expired");
                },
                recv(recv_event) -> res => {
                    match res {
                        Ok(evt) => {
                            tracing::debug!(event = ?evt, "Received event during connection establishment");
                            _ = me.act_event_up(evt).map_err(|error| {
                                tracing::error!(%error, "Failed to process event during connection establishment");
                            });
                        }
                        Err(error) => {
                            tracing::error!(%error, "Failed receiving event during connection establishment");
                        }
                    }
                }
            }
        });
    }

    pub fn dismantle(&mut self) {
        let mut me = self.clone();
        thread::spawn(move || {
            match me.establish_channel.0.send(()) {
                Ok(()) => (),
                Err(error) => {
                    tracing::error!(%error, "Critical error sending dismantle signal on establish channel - halting");
                    return;
                }
            }
            me.phase_up = crossbeam_channel::select! {
                recv(me.dismantle_channel.1) -> res => {
                    match res {
                        Ok(data) => data,
                        Err(error) => {
                            tracing::error!(%error, "Critical error receiving runtime data on dismantle channel - halting");
                            return;
                        }
                    }
                }
                default(Duration::from_secs(3)) => {
                            tracing::error!("Critical timeout receiving runtime data on dismantle channel - halting");
                            return;
                }
            };
            me.phase_down = me.phase_up.clone().into();

            let mut me2 = me.clone();
            thread::spawn(move || loop {
                let recv_event = me2.act_down();
                crossbeam_channel::select! {
                    recv(me2.abort_channel.1) -> res => {
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
                                tracing::debug!(event = ?evt, "Received event during connection dismantling");
                                _ = me2.act_event_down(evt).map_err(|error| {
                                    tracing::error!(%error, "Failed to process event during connection dismantling");
                                });
                            }
                            Err(error) => {
                                tracing::error!(%error, "Failed receiving event during connection dismantling");
                            }
                        }
                    }
                }
            });
        });
    }

    pub fn abort(&self) {
        tracing::info!("Aborting connection");
        self.abort_channel.0.send(()).unwrap_or_else(|error| {
            tracing::error!(%error, "Failed sending signal on abort channel");
        });
    }

    pub fn pretty_print_path(&self) -> String {
        format!("(entry){}", self.destination.pretty_print_path())
    }

    fn act_up(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        tracing::info!(phase = %self.phase_up, "Establishing connection");
        match self.phase_up.clone() {
            PhaseUp::Idle => self.idle2bridge(),
            PhaseUp::BridgeSessionOpen(session) => self.bridge2wgreg(&session),
            PhaseUp::WgRegistrationReceived(session, _registration) => self.wgreg2teardown(&session),
            PhaseUp::BridgeSessionClosed(_registration) => self.teardown2main(),
            PhaseUp::MainSessionOpen(session, _since, _registration) => self.monitor(&session),
        }
    }

    fn act_down(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        tracing::info!(phase_down = %self.phase_down, "Dismantling connection");
        match self.phase_down.clone() {
            PhaseDown::Idle => crossbeam_channel::never(),
            PhaseDown::MainSessionOpen(session, _since, _registration) => self.main2teardown(&session),
            PhaseDown::MainSessionClosed(_registration) => self.teardown2bridge(),
            PhaseDown::BridgeSessionOpen(session, _registration) => self.bridge2wgunreg(&session),
            PhaseDown::WgUnregistrationReceived(session) => self.wgunreg2teardown(&session),
            PhaseDown::BridgeSessionClosed => crossbeam_channel::never(),
        }
    }

    fn act_event_up(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            InternalEvent::SetUpBridgeSession(res) => {
                let session = res?;
                if let PhaseUp::Idle = self.phase_up {
                    self.phase_up = PhaseUp::BridgeSessionOpen(session);
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            /*
                    // Some(Object {"status": String("LISTEN_HOST_ALREADY_USED")}) })))
                    Err(session::Error::RemoteData(remote_data::CustomError {
                        status: Some(StatusCode::CONFLICT),
                        value: Some(json),
                        reqw_err: _,
                    })) => {
                        if json["status"] == "LISTEN_HOST_ALREADY_USED" {
                            // TODO hanlde dismantling on existing port
                        }
                        tracing::error!(?json, "Failed to open bridge session - CONFLICT");
                    }
                    Err(error) => {
                        tracing::error!(%error, "Failed to open bridge session");
                    }
                },
            }
                    */
            InternalEvent::RegisterWg(res) => {
                let registration = res?;
                if let PhaseUp::BridgeSessionOpen(session) = self.phase_up.clone() {
                    self.phase_up = PhaseUp::WgRegistrationReceived(session, registration);
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            InternalEvent::TearDownBridgeSession(res) => {
                res?;
                if let PhaseUp::WgRegistrationReceived(_session, registration) = self.phase_up.clone() {
                    self.phase_up = PhaseUp::BridgeSessionClosed(registration);
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            InternalEvent::SetUpMainSession(res) => {
                let session = res?;
                if let PhaseUp::BridgeSessionClosed(registration) = self.phase_up.clone() {
                    self.phase_up = PhaseUp::MainSessionOpen(session.clone(), SystemTime::now(), registration.clone());
                    let host = self
                        .entry_node
                        .endpoint
                        .host()
                        .map(|host| host.to_string())
                        .unwrap_or("".to_string());
                    _ = self.sender.send(Event::Connected(ConnectInfo {
                        endpoint: format!("{}:{}", host, session.port),
                        registration: registration.clone(),
                    }));
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            InternalEvent::ListSessions(res) => {
                let sessions = res?;

                if let PhaseUp::MainSessionOpen(session, since, registration) = self.phase_up.clone() {
                    if session.verify_open(&sessions) {
                        tracing::info!(%session, "session verified open since {}", log_output::elapsed(&since));
                        Ok(())
                    } else {
                        tracing::info!("Session is closed");
                        self.phase_up = PhaseUp::BridgeSessionClosed(registration);
                        self.sender.send(Event::Disconnected).map_err(InternalError::SendError)
                    }
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            evt => Err(InternalError::UnexecptedEvent(evt)),
        }
    }

    fn act_event_down(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            InternalEvent::TearDownMainSession(res) => {
                res?;
                if let PhaseDown::MainSessionOpen(_session, _since, registration) = self.phase_down.clone() {
                    self.phase_down = PhaseDown::MainSessionClosed(registration);
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            InternalEvent::SetUpBridgeSession(res) => {
                let session = res?;
                if let PhaseDown::MainSessionClosed(registration) = self.phase_down.clone() {
                    self.phase_down = PhaseDown::BridgeSessionOpen(session, registration);
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            InternalEvent::UnregisterWg(res) => {
                res?;
                if let PhaseDown::BridgeSessionOpen(session, _registration) = self.phase_down.clone() {
                    self.phase_down = PhaseDown::WgUnregistrationReceived(session);
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            InternalEvent::TearDownBridgeSession(res) => {
                res?;
                if let PhaseDown::WgUnregistrationReceived(_session) = self.phase_down.clone() {
                    self.phase_down = PhaseDown::BridgeSessionClosed;
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            evt => Err(InternalError::UnexecptedEvent(evt)),
        }
    }

    /// Transition from Idle to BridgeSessionOpen
    fn idle2bridge(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
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
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        thread::spawn(move || {
            let res = Session::open(&client, &params);
            _ = s.send(InternalEvent::SetUpBridgeSession(res));
        });
        r
    }

    /// Transition from BridgeSessionOpen to WgRegistrationReceived
    fn bridge2wgreg(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        let ri = wg_client::Input::new(&self.wg_public_key, &self.entry_node.endpoint, session);
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = wg_client::register(&client, &ri);
            _ = s.send(InternalEvent::RegisterWg(res));
        });
        r
    }

    /// Transition from WgRegistrationReceived to BridgeSessionClosed
    fn wgreg2teardown(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        let params = session::CloseSession::new(&self.entry_node, &Duration::from_secs(15));
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        let session = session.clone();
        thread::spawn(move || {
            let res = session.close(&client, &params);
            _ = s.send(InternalEvent::TearDownBridgeSession(res));
        });
        r
    }

    /// Transition from BridgeSessionClosed to MainSessionOpen
    fn teardown2main(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
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
            _ = s.send(InternalEvent::SetUpMainSession(res));
        });
        r
    }

    /// Looping state in MainSessionOpen
    fn monitor(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
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
        r
    }

    /// Transition from MainSessionOpen to MainSessionClosed
    fn main2teardown(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        let params = session::CloseSession::new(&self.entry_node, &Duration::from_secs(10));
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        let session = session.clone();
        thread::spawn(move || {
            let res = session.close(&client, &params);
            _ = s.send(InternalEvent::TearDownMainSession(res));
        });
        r
    }

    /// Transition from MainSessionClosed to BridgeSessionOpen
    fn teardown2bridge(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let params = session::OpenSession::bridge(
            &self.entry_node,
            &self.destination.peer_id,
            &self.destination.bridge.capabilities,
            &self.destination.path,
            &self.destination.bridge.target,
            &Duration::from_secs(10),
        );
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = Session::open(&client, &params);
            _ = s.send(InternalEvent::SetUpBridgeSession(res));
        });
        r
    }

    /// Transition from BridgeSessionOpen to WgUnregistrationReceived
    fn bridge2wgunreg(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        let params = wg_client::Input::new(&self.wg_public_key, &self.entry_node.endpoint, session);
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = wg_client::unregister(&client, &params);
            _ = s.send(InternalEvent::UnregisterWg(res));
        });
        r
    }

    /// Transition from WgUnregistrationReceived to BridgeSessionClosed
    fn wgunreg2teardown(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        let params = session::CloseSession::new(&self.entry_node, &Duration::from_secs(10));
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        let session = session.clone();
        thread::spawn(move || {
            let res = session.close(&client, &params);
            _ = s.send(InternalEvent::TearDownBridgeSession(res));
        });
        r
    }
}

impl Display for PhaseUp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PhaseUp::Idle => write!(f, "Idle"),
            PhaseUp::BridgeSessionOpen(session) => write!(f, "BridgeSessionOpen({})", session),
            PhaseUp::WgRegistrationReceived(session, registration) => {
                write!(f, "WgRegistrationReceived({}, {})", session, registration)
            }
            PhaseUp::BridgeSessionClosed(registration) => write!(f, "BridgeSessionClosed({})", registration),
            PhaseUp::MainSessionOpen(session, since, registration) => write!(
                f,
                "MainSessionOpen({}, since {}, {})",
                session,
                log_output::elapsed(since),
                registration
            ),
        }
    }
}

impl Display for PhaseDown {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PhaseDown::Idle => write!(f, "Idle"),
            PhaseDown::MainSessionOpen(session, since, registration) => write!(
                f,
                "MainSessionOpen({}, since {}, {})",
                session,
                log_output::elapsed(since),
                registration
            ),
            PhaseDown::MainSessionClosed(registration) => write!(f, "MainSessionClosed({})", registration),
            PhaseDown::BridgeSessionOpen(session, registration) => {
                write!(f, "BridgeSessionOpen({}, {})", session, registration)
            }
            PhaseDown::WgUnregistrationReceived(session) => write!(f, "WgUnregistrationReceived({})", session),
            PhaseDown::BridgeSessionClosed => write!(f, "BridgeSessionClosed"),
        }
    }
}

impl From<PhaseUp> for PhaseDown {
    fn from(phase_up: PhaseUp) -> Self {
        match phase_up {
            PhaseUp::Idle => PhaseDown::BridgeSessionClosed,
            PhaseUp::BridgeSessionOpen(session) => PhaseDown::WgUnregistrationReceived(session),
            PhaseUp::WgRegistrationReceived(session, registration) => {
                PhaseDown::BridgeSessionOpen(session, registration)
            }
            PhaseUp::BridgeSessionClosed(registration) => PhaseDown::MainSessionClosed(registration),
            PhaseUp::MainSessionOpen(session, since, registration) => {
                PhaseDown::MainSessionOpen(session, since, registration)
            }
        }
    }
}
