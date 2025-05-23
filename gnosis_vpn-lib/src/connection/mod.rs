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
use crate::monitor;
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
    #[error("Failed to send message: {0}")]
    ChannelError(#[from] crossbeam_channel::SendError<()>),
}

/// Represents the different phases of establishing a connection.
#[derive(Clone, Debug)]
enum PhaseUp {
    Ready,
    FixBridgeSession,
    FixBridgeSessionClosing(Session),
    WgRegistration(Session),
    CloseBridgeSession(Session, Registration),
    PrepareMainSession(Registration),
    FixMainSession(Registration),
    MonitorMainSession(Session, SystemTime, Registration),
    MainSessionBroken(Session, Registration),
}

/// Represents the different phases of dismantling a connection.
#[derive(Clone, Debug)]
enum PhaseDown {
    CloseMainSession(Session, SystemTime, Registration),
    PrepareBridgeSession(Registration),
    WgUnregistration(Session, Registration),
    CloseBridgeSession(Session),
    Retired,
}

#[derive(Debug)]
enum InternalEvent {
    OpenSession(Result<Session, session::Error>),
    CloseSession(Result<(), session::Error>),
    ListSessions(Result<Vec<Session>, session::Error>),
    RegisterWg(Result<Registration, wg_client::Error>),
    UnregisterWg(Result<(), wg_client::Error>),
    Ping(Result<(), monitor::Error>),
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
    #[error("External session error: {0}")]
    SessionError(#[from] session::Error),
    #[error("External GnosisVPN error: {0}")]
    WgError(#[from] wg_client::Error),
    #[error("Channel send error: {0}")]
    SendError(#[from] crossbeam_channel::SendError<Event>),
    #[error("Unexpected event: {0}")]
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
            phase_down: PhaseDown::Retired,
            phase_up: PhaseUp::Ready,
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
            // Backoff handling
            // Inactive - no backoff was set, act up
            // Active - backoff was set and can trigger, don't act until backoff delay
            // Triggered - backoff was triggered, time to act up again keeping backoff active
            let (recv_event, recv_backoff) = match me.backoff {
                BackoffState::Inactive => (me.act_up(), crossbeam_channel::never()),
                BackoffState::Active(mut backoff) => match backoff.next_backoff() {
                    Some(delay) => {
                        tracing::debug!(?backoff, delay = ?delay, "Triggering backoff delay during connection establishment");
                        me.backoff = BackoffState::Triggered(backoff);
                        (crossbeam_channel::never(), crossbeam_channel::after(delay))
                    }
                    None => {
                        me.backoff = BackoffState::Inactive;
                        tracing::error!("Critical error: backoff exhausted during connection establishment - halting");
                        _ = me.sender.send(Event::Dismantled).map_err(|error| {
                            tracing::error!(%error, "Failed sending dismantled event");
                        });
                        break;
                    }
                },
                BackoffState::Triggered(backoff) => {
                    tracing::debug!(?backoff, "Activating backoff during connection establishment");
                    me.backoff = BackoffState::Active(backoff);
                    (me.act_up(), crossbeam_channel::never())
                }
            };
            // main listening loop
            crossbeam_channel::select! {
                // waiting on dismantle signal for providing runtime data
                recv(me.establish_channel.1) -> res => {
                    match res {
                        Ok(()) => {
                            match me.dismantle_channel.0.send(me.phase_up) {
                                Ok(()) => (),
                                Err(error) => {
                                    tracing::error!(%error, "Critical error sending connection data on dismantle channel - halting");
                                    _ = me.sender.send(Event::Dismantled).map_err(|error| {
                                        tracing::error!(%error, "Failed sending dismantled event");
                                    });
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
                    tracing::debug!("Backoff delay expired during connection establishment");
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
        let mut outer = self.clone();
        thread::spawn(move || {
            // abort establishing
            match outer.establish_channel.0.send(()) {
                Ok(()) => (),
                Err(error) => {
                    tracing::error!(%error, "Critical error sending dismantle signal on establish channel - halting");
                    return;
                }
            }
            outer.phase_up = crossbeam_channel::select! {
                recv(outer.dismantle_channel.1) -> res => {
                    match res {
                        Ok(data) => data,
                        Err(error) => {
                            tracing::error!(%error, "Critical error receiving runtime data on dismantle channel - halting");
                            return;
                        }
                    }
                }
                default(Duration::from_secs(5)) => {
                            tracing::error!("Critical timeout receiving connection data on dismantle channel - halting");
                            return;
                }
            };
            outer.phase_down = outer.phase_up.clone().into();

            let mut me = outer.clone();
            thread::spawn(move || loop {
                // Backoff handling
                // Inactive - no backoff was set, act up
                // Active - backoff was set and can trigger, don't act until backoff delay
                // Triggered - backoff was triggered, time to act up again keeping backoff active
                let (recv_event, recv_backoff) = match me.backoff {
                    BackoffState::Inactive => (me.act_down(), crossbeam_channel::never()),
                    BackoffState::Active(mut backoff) => match backoff.next_backoff() {
                        Some(delay) => {
                            tracing::debug!(?backoff, delay = ?delay, "Triggering backoff delay during connection dismantling");
                            me.backoff = BackoffState::Triggered(backoff);
                            (crossbeam_channel::never(), crossbeam_channel::after(delay))
                        }
                        None => {
                            me.backoff = BackoffState::Inactive;
                            tracing::error!(
                                "Critical error: backoff exhausted during connection dismantling - halting"
                            );
                            _ = me.sender.send(Event::Dismantled).map_err(|error| {
                                tracing::error!(%error, "Failed sending dismantled event");
                            });
                            break;
                        }
                    },
                    BackoffState::Triggered(backoff) => {
                        tracing::debug!(?backoff, "Activating backoff during connection dismantling");
                        me.backoff = BackoffState::Active(backoff);
                        (me.act_down(), crossbeam_channel::never())
                    }
                };
                // main listening loop
                crossbeam_channel::select! {
                    recv(recv_backoff) -> _ => {
                        tracing::debug!("Backoff delay expired during connection dismantling");
                    }
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
                                tracing::debug!(event = ?evt, "Received event during connection dismantling");
                                _ = me.act_event_down(evt).map_err(|error| {
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
        _ = self.abort_channel.0.send(()).map_err(|error| {
            tracing::error!(%error, "Failed sending signal on abort channel");
        });
    }

    pub fn pretty_print_path(&self) -> String {
        format!("(entry){}", self.destination.pretty_print_path())
    }

    fn act_up(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        tracing::debug!(phase = %self.phase_up, "Establishing connection");
        match self.phase_up.clone() {
            PhaseUp::Ready => self.open_session(self.bridge_session_params()),
            PhaseUp::FixBridgeSession => self.list_sessions(&session::Protocol::Tcp),
            PhaseUp::FixBridgeSessionClosing(session) => self.close_session(&session),
            PhaseUp::WgRegistration(session) => self.register_wg(&session),
            PhaseUp::CloseBridgeSession(session, _registration) => self.close_session(&session),
            PhaseUp::PrepareMainSession(_registration) => self.open_session(self.main_session_params()),
            PhaseUp::FixMainSession(_registration) => self.list_sessions(&session::Protocol::Udp),
            PhaseUp::MonitorMainSession(_session, _since, _registration) => self.ping(),
            PhaseUp::MainSessionBroken(session, _registration) => self.close_session(&session),
        }
    }

    fn act_down(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        tracing::debug!(phase_down = %self.phase_down, "Dismantling connection");
        match self.phase_down.clone() {
            PhaseDown::CloseMainSession(session, _since, _registration) => self.close_session(&session),
            PhaseDown::PrepareBridgeSession(_registration) => self.open_session(self.bridge_session_params()),
            PhaseDown::WgUnregistration(session, _registration) => self.unregister_wg(&session),
            PhaseDown::CloseBridgeSession(session) => self.close_session(&session),
            PhaseDown::Retired => self.shutdown(),
        }
    }

    fn act_event_up(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            InternalEvent::OpenSession(res) => {
                let listen_host_used = matches!(res, Err(session::Error::ListenHostAlreadyUsed));
                match self.phase_up.clone() {
                    PhaseUp::Ready => {
                        if listen_host_used {
                            tracing::warn!("Listen host already used - trying to close existing session");
                            self.phase_up = PhaseUp::FixBridgeSession;
                            self.backoff = BackoffState::Inactive;
                            return Ok(());
                        };
                        self.phase_up = PhaseUp::WgRegistration(res?.clone());
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    PhaseUp::PrepareMainSession(registration) => {
                        if listen_host_used {
                            tracing::warn!("Listen host already used - trying to close existing session");
                            self.phase_up = PhaseUp::FixMainSession(registration.clone());
                            self.backoff = BackoffState::Inactive;
                            return Ok(());
                        };
                        let session = res?;
                        self.phase_up =
                            PhaseUp::MonitorMainSession(session.clone(), SystemTime::now(), registration.clone());
                        self.backoff = BackoffState::Inactive;
                        let host = self
                            .entry_node
                            .endpoint
                            .host()
                            .map(|host| host.to_string())
                            .unwrap_or("".to_string());
                        self.sender
                            .send(Event::Connected(ConnectInfo {
                                endpoint: format!("{}:{}", host, session.port),
                                registration: registration.clone(),
                            }))
                            .map_err(InternalError::SendError)
                    }
                    _ => Err(InternalError::UnexpectedPhase),
                }
            }
            InternalEvent::RegisterWg(res) => {
                if let PhaseUp::WgRegistration(session) = self.phase_up.clone() {
                    self.phase_up = PhaseUp::CloseBridgeSession(session, res?);
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            InternalEvent::CloseSession(res) => {
                // assume session is closed when not found
                let not_found = matches!(res, Err(session::Error::SessionNotFound));
                if !not_found {
                    res?;
                }
                match self.phase_up.clone() {
                    PhaseUp::CloseBridgeSession(_session, registration) => {
                        self.phase_up = PhaseUp::PrepareMainSession(registration);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    PhaseUp::MainSessionBroken(_session, registration) => {
                        self.phase_up = PhaseUp::PrepareMainSession(registration);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    PhaseUp::FixBridgeSessionClosing(_session) => {
                        self.phase_up = PhaseUp::Ready;
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    _ => Err(InternalError::UnexpectedPhase),
                }
            }
            InternalEvent::Ping(res) =>
            {
                #[allow(clippy::collapsible_else_if)]
                if res.is_ok() {
                    if let PhaseUp::MonitorMainSession(session, since, _registration) = self.phase_up.clone() {
                        tracing::info!(%session, "session verified open for {}", log_output::elapsed(&since));
                        Ok(())
                    } else {
                        Err(InternalError::UnexpectedPhase)
                    }
                } else {
                    if let PhaseUp::MonitorMainSession(session, _since, registration) = self.phase_up.clone() {
                        tracing::warn!(%session, "Session ping failed");
                        self.phase_up = PhaseUp::MainSessionBroken(session, registration);
                        self.sender.send(Event::Disconnected).map_err(InternalError::SendError)
                    } else {
                        Err(InternalError::UnexpectedPhase)
                    }
                }
            }
            InternalEvent::ListSessions(res) => {
                let sessions = res?;
                let open_session = sessions.iter().find(|s| self.entry_node.conflicts_listen_host(s));
                match open_session {
                    Some(session) => {
                        tracing::info!(%session, "Found conflicting session - closing");
                        self.phase_up = PhaseUp::FixBridgeSessionClosing(session.clone());
                        self.backoff = BackoffState::Inactive;
                    }
                    None => {
                        tracing::info!("No conflicting session found - proceed as normal");
                        self.phase_up = PhaseUp::Ready;
                        self.backoff = BackoffState::Inactive;
                    }
                };
                Ok(())
            }
            InternalEvent::UnregisterWg(res) => Err(InternalError::UnexecptedEvent(InternalEvent::UnregisterWg(res))),
        }
    }

    fn act_event_down(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            InternalEvent::OpenSession(res) => {
                let session = res?;
                if let PhaseDown::PrepareBridgeSession(registration) = self.phase_down.clone() {
                    self.phase_down = PhaseDown::WgUnregistration(session, registration);
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            InternalEvent::CloseSession(res) => {
                res?;
                match self.phase_down.clone() {
                    PhaseDown::CloseMainSession(_session, _since, registration) => {
                        self.phase_down = PhaseDown::PrepareBridgeSession(registration);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    PhaseDown::CloseBridgeSession(_session) => {
                        self.phase_down = PhaseDown::Retired;
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    _ => Err(InternalError::UnexpectedPhase),
                }
            }
            InternalEvent::UnregisterWg(res) => {
                res?;
                if let PhaseDown::WgUnregistration(session, _registration) = self.phase_down.clone() {
                    self.phase_down = PhaseDown::CloseBridgeSession(session);
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            evt => Err(InternalError::UnexecptedEvent(evt)),
        }
    }

    fn register_wg(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        let ri = wg_client::Input::new(&self.wg_public_key, &self.entry_node.endpoint, session);
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        thread::spawn(move || {
            let res = wg_client::register(&client, &ri);
            _ = s.send(InternalEvent::RegisterWg(res));
        });
        r
    }

    fn open_session(&mut self, params: session::OpenSession) -> crossbeam_channel::Receiver<InternalEvent> {
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        thread::spawn(move || {
            let res = Session::open(&client, &params);
            _ = s.send(InternalEvent::OpenSession(res));
        });
        r
    }

    fn ping(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let mut rng = rand::thread_rng();
            let delay = Duration::from_secs(rng.gen_range(5..10));
            let after = crossbeam_channel::after(delay);
            crossbeam_channel::select! {
                recv(after) -> _ => {
                    let res = monitor::ping();
            _ = s.send(InternalEvent::Ping(res));
                }
            }
        });
        r
    }

    fn list_sessions(&mut self, protocol: &session::Protocol) -> crossbeam_channel::Receiver<InternalEvent> {
        let params = session::ListSession::new(&self.entry_node, protocol);
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        thread::spawn(move || {
            let res = Session::list(&client, &params);
            _ = s.send(InternalEvent::ListSessions(res));
        });
        r
    }

    fn unregister_wg(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        let params = wg_client::Input::new(&self.wg_public_key, &self.entry_node.endpoint, session);
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        thread::spawn(move || {
            let res = wg_client::unregister(&client, &params);
            _ = s.send(InternalEvent::UnregisterWg(res));
        });
        r
    }

    fn close_session(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        let params = session::CloseSession::new(&self.entry_node);
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        let session = session.clone();
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        thread::spawn(move || {
            let res = session.close(&client, &params);
            _ = s.send(InternalEvent::CloseSession(res));
        });
        r
    }

    /// Final state before dropping
    fn shutdown(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        _ = self.sender.send(Event::Dismantled).map_err(|error| {
            tracing::error!(%error, "Failed sending dismantled event");
        });
        crossbeam_channel::never()
    }

    fn bridge_session_params(&self) -> session::OpenSession {
        session::OpenSession::bridge(
            self.entry_node.clone(),
            self.destination.peer_id,
            self.destination.bridge.capabilities.clone(),
            self.destination.path.clone(),
            self.destination.bridge.target.clone(),
        )
    }

    fn main_session_params(&self) -> session::OpenSession {
        session::OpenSession::main(
            self.entry_node.clone(),
            self.destination.peer_id,
            self.destination.wg.capabilities.clone(),
            self.destination.path.clone(),
            self.destination.wg.target.clone(),
        )
    }
}

impl Display for PhaseUp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PhaseUp::Ready => write!(f, "Ready"),
            PhaseUp::FixBridgeSession => write!(f, "FixBridgeSession"),
            PhaseUp::FixBridgeSessionClosing(session) => write!(f, "FixBridgeSessionClosing({})", session),
            PhaseUp::WgRegistration(session) => write!(f, "WgRegistration({})", session),
            PhaseUp::CloseBridgeSession(session, registration) => {
                write!(f, "CloseBridgeSession({}, {})", session, registration)
            }
            PhaseUp::PrepareMainSession(registration) => write!(f, "PrepareMainSession({})", registration),
            PhaseUp::FixMainSession(registration) => write!(f, "FixMainSession({})", registration),
            PhaseUp::MonitorMainSession(session, since, registration) => write!(
                f,
                "MonitorMainSession({}, since {}, {})",
                session,
                log_output::elapsed(since),
                registration
            ),
            PhaseUp::MainSessionBroken(session, registration) => {
                write!(f, "MainSessionBroken({}, {})", session, registration)
            }
        }
    }
}

impl Display for PhaseDown {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PhaseDown::CloseMainSession(session, since, registration) => write!(
                f,
                "CloseMainSession({}, since {}, {})",
                session,
                log_output::elapsed(since),
                registration
            ),
            PhaseDown::PrepareBridgeSession(registration) => write!(f, "PrepareBridgeSession({})", registration),
            PhaseDown::WgUnregistration(session, registration) => {
                write!(f, "WgUnregistration({}, {})", session, registration)
            }
            PhaseDown::CloseBridgeSession(session) => write!(f, "CloseBridgeSession({})", session),
            PhaseDown::Retired => write!(f, "Retired"),
        }
    }
}

impl From<PhaseUp> for PhaseDown {
    fn from(phase_up: PhaseUp) -> Self {
        match phase_up {
            PhaseUp::Ready => PhaseDown::Retired,
            PhaseUp::FixBridgeSession => PhaseDown::Retired,
            PhaseUp::FixBridgeSessionClosing(_session) => PhaseDown::Retired,
            PhaseUp::WgRegistration(session) => PhaseDown::CloseBridgeSession(session),
            PhaseUp::CloseBridgeSession(session, registration) => PhaseDown::WgUnregistration(session, registration),
            PhaseUp::PrepareMainSession(registration) => PhaseDown::PrepareBridgeSession(registration),
            PhaseUp::FixMainSession(registration) => PhaseDown::PrepareBridgeSession(registration),
            PhaseUp::MonitorMainSession(session, since, registration) => {
                PhaseDown::CloseMainSession(session, since, registration)
            }
            PhaseUp::MainSessionBroken(_session, registration) => PhaseDown::PrepareBridgeSession(registration),
        }
    }
}

impl Display for InternalEvent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            InternalEvent::OpenSession(res) => write!(f, "OpenSession({:?})", res),
            InternalEvent::CloseSession(res) => write!(f, "CloseSession({:?})", res),
            InternalEvent::RegisterWg(res) => write!(f, "RegisterWg({:?})", res),
            InternalEvent::UnregisterWg(res) => write!(f, "UnregisterWg({:?})", res),
            InternalEvent::Ping(res) => write!(f, "Ping({:?})", res),
            InternalEvent::ListSessions(res) => write!(f, "ListSessions({:?})", res),
        }
    }
}
