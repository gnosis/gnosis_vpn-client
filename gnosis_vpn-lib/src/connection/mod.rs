use backoff::{ExponentialBackoff, ExponentialBackoffBuilder, backoff::Backoff};
use std::fmt::{self, Display};
use std::thread;
use std::time::{Duration, SystemTime};

use crossbeam_channel;
use rand::Rng;
use reqwest::blocking;
use thiserror::Error;

use crate::entry_node::{self, EntryNode};
use crate::gvpn_client::{self, Registration};
use crate::log_output;
use crate::monitor;
use crate::session::{self, Protocol, Session};
use crate::wg_tooling;

pub use destination::{Destination, SessionParameters};

pub mod destination;

#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// Connection has been fully established and ping tested
    Connected,
    /// Currently not connected
    Disconnected,
    /// Connection is broken and should be dismantled
    Broken,
    /// Connection has reached final state
    Dismantled,
}

#[derive(Clone, Debug)]
pub struct ConnectInfo {
    pub endpoint: String,
    pub registration: Registration,
}

/// Represents the different phases of establishing a connection.
#[derive(Clone, Debug)]
enum PhaseUp {
    Ready,
    FixBridgeSession,
    FixBridgeSessionClosing(Session),
    WgRegistration(Session),
    CloseBridgeSession(Session, Registration),
    PreparePingSession(Registration),
    FixPingSession(Registration),
    FixPingSessionClosing(Session, Registration),
    PreparePingTunnel(Session, Registration),
    CheckPingTunnel(Session, Registration),
    ClosePingTunnel(Session, Registration),
    PrepareMainSession(Registration),
    FixMainSession(Registration),
    FixMainSessionClosing(Session, Registration),
    PrepareMainTunnel(Session, Registration, SystemTime),
    TunnelEstablished(Session, Registration, SystemTime),
    MonitorTunnel(Session, Registration, SystemTime),
    TunnelBroken(Session, Registration),
}

/// Represents the different phases of dismantling a connection.
#[derive(Clone, Debug)]
enum PhaseDown {
    CloseTunnel(Session, Registration),
    PrepareBridgeSession(Registration),
    FixBridgeSession(Registration),
    FixBridgeSessionClosing(Session, Registration),
    WgUnregistration(Session, Registration),
    CloseBridgeSession(Session),
    Retired,
}

#[derive(Debug)]
enum InternalEvent {
    OpenSession(Result<Session, session::Error>),
    CloseSession(Result<(), session::Error>),
    ListSessions(Result<Vec<Session>, session::Error>),
    RegisterWg(Result<Registration, gvpn_client::Error>),
    UnregisterWg(Result<(), gvpn_client::Error>),
    WgOpenTunnel(WgOpenResult),
    Ping(Result<(), monitor::Error>),
}

#[derive(Debug)]
enum WgOpenResult {
    EntryNode(entry_node::Error),
    WgTooling(wg_tooling::Error),
    Ok,
}

#[derive(Clone, Debug)]
enum BackoffState {
    Inactive,
    Active(ExponentialBackoff),
    Triggered(ExponentialBackoff),
    NotRecoverable(String),
}

#[derive(Clone, Debug)]
pub struct Connection {
    // message passing helper
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
    wg: wg_tooling::WireGuard,
    sender: crossbeam_channel::Sender<Event>,
}

#[derive(Debug, Error)]
enum InternalError {
    #[error("Invalid phase for action")]
    UnexpectedPhase,
    #[error("External session error: {0}")]
    SessionError(#[from] session::Error),
    #[error("External Gnosis VPN error: {0}")]
    WgError(#[from] gvpn_client::Error),
    #[error("Channel send error: {0}")]
    SendError(#[from] crossbeam_channel::SendError<Event>),
    #[error("Entry node error: {0}")]
    EntryNodeError(#[from] crate::entry_node::Error),
    #[error("WireGuard error: {0}")]
    WireGuard(#[from] wg_tooling::Error),
    #[error("Unexpected event: {0}")]
    UnexpectedEvent(Box<InternalEvent>),
}

impl Connection {
    pub fn new(
        entry_node: EntryNode,
        destination: Destination,
        wg: wg_tooling::WireGuard,
        sender: crossbeam_channel::Sender<Event>,
    ) -> Self {
        Connection {
            destination,
            entry_node,
            sender,
            wg,
            backoff: BackoffState::Inactive,
            client: blocking::Client::new(),
            dismantle_channel: crossbeam_channel::bounded(1),
            establish_channel: crossbeam_channel::bounded(1),
            phase_down: PhaseDown::Retired,
            phase_up: PhaseUp::Ready,
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
        thread::spawn(move || {
            loop {
                // Backoff handling
                // Inactive - no backoff was set, act up
                // Active - backoff was set and can trigger, don't act until backoff delay
                // Triggered - backoff was triggered, time to act up again keeping backoff active
                // NotRecoverable - critical error, halt connection establishment
                let (recv_event, recv_backoff) = match me.backoff.clone() {
                    BackoffState::Inactive => (me.act_up(), crossbeam_channel::never()),
                    BackoffState::Active(mut backoff) => match backoff.next_backoff() {
                        Some(delay) => {
                            tracing::debug!(?backoff, delay = ?delay, "Triggering backoff delay during connection establishment");
                            me.backoff = BackoffState::Triggered(backoff);
                            (crossbeam_channel::never(), crossbeam_channel::after(delay))
                        }
                        None => {
                            me.backoff = BackoffState::Inactive;
                            tracing::error!("Critical error: backoff exhausted during connection establishment");
                            _ = me.sender.send(Event::Broken).map_err(|error| {
                                tracing::error!(%error, "Failed sending broken event");
                            });
                            (crossbeam_channel::never(), crossbeam_channel::never())
                        }
                    },
                    BackoffState::Triggered(backoff) => {
                        tracing::debug!(?backoff, "Activating backoff during connection establishment");
                        me.backoff = BackoffState::Active(backoff);
                        (me.act_up(), crossbeam_channel::never())
                    }
                    BackoffState::NotRecoverable(error) => {
                        tracing::error!(%error, "Critical error during connection establishment - halting");
                        _ = me.sender.send(Event::Broken).map_err(|error| {
                            tracing::error!(%error, "Failed sending dismantled event");
                        });
                        (crossbeam_channel::never(), crossbeam_channel::never())
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
                                        tracing::error!(%error, "Critical error: sending connection data on dismantle channel");
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
                    recv(recv_backoff) -> _ => {
                        tracing::debug!("Backoff delay expired during connection establishment");
                    },
                    recv(recv_event) -> res => {
                        match res {
                            Ok(event) => {
                                tracing::debug!(%event, "Received event during connection establishment");
                                _ = me.act_event_up(event).map_err(|error| {
                                    tracing::error!(%error, "Failed to process event during connection establishment");
                                });
                            }
                            Err(error) => {
                                tracing::error!(%error, "Failed receiving event during connection establishment");
                            }
                        }
                    }
                }
            }
        });
    }

    pub fn dismantle(&mut self) {
        let mut outer = self.clone();
        thread::spawn(move || {
            // cancel establishing connection
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
            thread::spawn(move || {
                loop {
                    // Backoff handling
                    // Inactive - no backoff was set, act up
                    // Active - backoff was set and can trigger, don't act until backoff delay
                    // Triggered - backoff was triggered, time to act up again keeping backoff active
                    // NotRecoverable - critical error, halt connection dismantling
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
                        BackoffState::NotRecoverable(error) => {
                            tracing::error!(%error, "Critical error during connection dismantling - halting");
                            _ = me.sender.send(Event::Dismantled).map_err(|error| {
                                tracing::error!(%error, "Failed sending dismantled event");
                            });
                            break;
                        }
                    };
                    // main listening loop
                    crossbeam_channel::select! {
                        recv(recv_backoff) -> _ => {
                            tracing::debug!("Backoff delay expired during connection dismantling");
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
                }
            });
        });
    }

    pub fn pretty_print_path(&self) -> String {
        format!("(entry){}", self.destination.pretty_print_path())
    }

    fn act_up(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        tracing::debug!(phase = %self.phase_up, "Establishing connection");
        match self.phase_up.clone() {
            PhaseUp::Ready => self.open_session(self.bridge_session_params()),
            PhaseUp::FixBridgeSession => self.list_sessions(&Protocol::Tcp),
            PhaseUp::FixBridgeSessionClosing(session) => self.close_session(&session),
            PhaseUp::WgRegistration(session) => self.register_wg(&session),
            PhaseUp::CloseBridgeSession(session, _registration) => self.close_session(&session),
            PhaseUp::PreparePingSession(_registration) => self.open_session(self.ping_session_params()),
            PhaseUp::FixPingSession(_registration) => self.list_sessions(&Protocol::Udp),
            PhaseUp::FixPingSessionClosing(session, _registration) => self.close_session(&session),
            PhaseUp::PreparePingTunnel(session, registration) => self.open_wg_session(&session, &registration),
            PhaseUp::CheckPingTunnel(_session, _registration) => self.immediate_ping(),
            PhaseUp::ClosePingTunnel(session, _registration) => self.close_wg_session(&session),
            PhaseUp::PrepareMainSession(_registration) => self.open_session(self.main_session_params()),
            PhaseUp::FixMainSession(_registration) => self.list_sessions(&Protocol::Udp),
            PhaseUp::FixMainSessionClosing(session, _registration) => self.close_session(&session),
            PhaseUp::PrepareMainTunnel(session, registration, _since) => self.open_wg_session(&session, &registration),
            PhaseUp::TunnelEstablished(_session, _registration, _since) => self.immediate_ping(),
            PhaseUp::MonitorTunnel(_session, _registration, _since) => self.delayed_ping(),
            PhaseUp::TunnelBroken(session, _registration) => self.close_wg_session(&session),
        }
    }

    fn act_down(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        tracing::debug!(phase_down = %self.phase_down, "Dismantling connection");
        match self.phase_down.clone() {
            PhaseDown::CloseTunnel(session, _registration) => self.close_wg_session(&session),
            PhaseDown::PrepareBridgeSession(_registration) => self.open_session(self.bridge_session_params()),
            PhaseDown::FixBridgeSession(_registration) => self.list_sessions(&Protocol::Tcp),
            PhaseDown::FixBridgeSessionClosing(session, _registration) => self.close_session(&session),
            PhaseDown::WgUnregistration(session, _registration) => self.unregister_wg(&session),
            PhaseDown::CloseBridgeSession(session) => self.close_session(&session),
            PhaseDown::Retired => self.shutdown(),
        }
    }

    fn act_event_up(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            // handle open session event depending on phase
            InternalEvent::OpenSession(res) => {
                check_entry_node(&res);
                let listen_host_used = matches!(&res, Err(session::Error::ListenHostAlreadyUsed));
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
                    PhaseUp::PreparePingSession(registration) => {
                        if listen_host_used {
                            tracing::warn!("Listen host already used - trying to close existing session");
                            self.phase_up = PhaseUp::FixPingSession(registration.clone());
                            self.backoff = BackoffState::Inactive;
                            return Ok(());
                        };
                        let session = res?;
                        self.phase_up = PhaseUp::PreparePingTunnel(session, registration);
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
                        self.phase_up = PhaseUp::PrepareMainTunnel(session, registration, SystemTime::now());
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    _ => Err(InternalError::UnexpectedPhase),
                }
            }

            // handle wg open tunnel event depending on result and phase
            InternalEvent::WgOpenTunnel(res) => match (res, self.phase_up.clone()) {
                (WgOpenResult::EntryNode(error), _) => {
                    self.backoff = BackoffState::NotRecoverable(format!("{error}"));
                    Ok(())
                }
                (WgOpenResult::WgTooling(error), _) => {
                    self.backoff = BackoffState::NotRecoverable(format!("{error}"));
                    Ok(())
                }
                (WgOpenResult::Ok, PhaseUp::PreparePingTunnel(session, registration)) => {
                    self.phase_up = PhaseUp::CheckPingTunnel(session.clone(), registration.clone());
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                }
                (WgOpenResult::Ok, PhaseUp::PrepareMainTunnel(session, registration, _since)) => {
                    self.phase_up =
                        PhaseUp::TunnelEstablished(session.clone(), registration.clone(), SystemTime::now());
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                }
                (_, _) => Err(InternalError::UnexpectedPhase),
            },

            // handle wg registration event depending on phase
            InternalEvent::RegisterWg(res) => {
                if let PhaseUp::WgRegistration(session) = self.phase_up.clone() {
                    check_tcp_session(&res, session.port);
                    self.phase_up = PhaseUp::CloseBridgeSession(session, res?);
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }

            // handle close session event depending on phase
            InternalEvent::CloseSession(res) => {
                check_entry_node(&res);
                let session_closed = matches!(&res, Err(session::Error::SessionNotFound));
                if !session_closed {
                    res?;
                }
                match self.phase_up.clone() {
                    PhaseUp::CloseBridgeSession(_session, registration) => {
                        self.phase_up = PhaseUp::PreparePingSession(registration);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    PhaseUp::ClosePingTunnel(_session, registration) => {
                        self.phase_up = PhaseUp::PrepareMainSession(registration);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    PhaseUp::FixPingSessionClosing(_session, registration) => {
                        self.phase_up = PhaseUp::PreparePingSession(registration);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    PhaseUp::FixBridgeSessionClosing(_session) => {
                        self.phase_up = PhaseUp::Ready;
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    PhaseUp::FixMainSessionClosing(_session, registration) => {
                        self.phase_up = PhaseUp::PrepareMainSession(registration);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    PhaseUp::TunnelBroken(_session, registration) => {
                        self.phase_up = PhaseUp::PreparePingSession(registration);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    _ => Err(InternalError::UnexpectedPhase),
                }
            }

            // handle ping event depending on result and phase
            InternalEvent::Ping(res) => match (res, self.phase_up.clone()) {
                (Ok(_), PhaseUp::CheckPingTunnel(session, registration)) => {
                    tracing::info!(%session, "Ping tunnel verified");
                    self.phase_up = PhaseUp::ClosePingTunnel(session, registration);
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                }
                (Ok(_), PhaseUp::TunnelEstablished(session, registration, since)) => {
                    tracing::info!(%session, "Session verified as open");
                    log_output::print_session_established(&self.pretty_print_path());
                    self.phase_up = PhaseUp::MonitorTunnel(session, registration, since);
                    self.backoff = BackoffState::Inactive;
                    self.sender.send(Event::Connected).map_err(InternalError::SendError)
                }
                (Ok(_), PhaseUp::MonitorTunnel(session, _registration, since)) => {
                    tracing::info!(%session, "Session verified as open for {}", log_output::elapsed(&since));
                    Ok(())
                }
                (Err(error), PhaseUp::CheckPingTunnel(session, _registration)) => {
                    tracing::warn!(%session, %error, "Ping during initial check failed");
                    if !error.would_block() {
                        log_output::print_port_instructions(session.port, Protocol::Udp);
                    }
                    Ok(())
                }
                (Err(error), PhaseUp::TunnelEstablished(session, _registration, _since)) => {
                    tracing::warn!(%session, %error, "Initial tunnel ping failed");
                    if !error.would_block() {
                        log_output::print_port_instructions(session.port, Protocol::Udp);
                    }
                    Ok(())
                }
                (Err(_), PhaseUp::MonitorTunnel(session, registration, since)) => {
                    tracing::warn!(%session, "Session ping failed after {}", log_output::elapsed(&since));
                    self.phase_up = PhaseUp::TunnelBroken(session, registration);
                    self.backoff = BackoffState::Inactive;
                    self.sender.send(Event::Disconnected).map_err(InternalError::SendError)
                }
                _ => Err(InternalError::UnexpectedPhase),
            },

            // handle list session event depending on phase
            InternalEvent::ListSessions(res) => {
                check_entry_node(&res);
                let sessions = res?;
                let open_session = sessions.iter().find(|s| self.entry_node.conflicts_listen_host(s));
                match (open_session, self.phase_up.clone()) {
                    (Some(session), PhaseUp::FixBridgeSession) => {
                        tracing::info!(%session, "Found conflicting session - closing");
                        self.phase_up = PhaseUp::FixBridgeSessionClosing(session.clone());
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    (Some(session), PhaseUp::FixPingSession(reg)) => {
                        tracing::info!(%session, "Found conflicting session - closing");
                        self.phase_up = PhaseUp::FixPingSessionClosing(session.clone(), reg);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    (Some(session), PhaseUp::FixMainSession(reg)) => {
                        tracing::info!(%session, "Found conflicting session - closing");
                        self.phase_up = PhaseUp::FixMainSessionClosing(session.clone(), reg);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    (None, PhaseUp::FixBridgeSession) => {
                        tracing::info!("No conflicting session found - proceed as normal");
                        self.phase_up = PhaseUp::Ready;
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    (None, PhaseUp::FixPingSession(reg)) => {
                        tracing::info!("No conflicting session found - proceed as normal");
                        self.phase_up = PhaseUp::PreparePingSession(reg);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    (None, PhaseUp::FixMainSession(reg)) => {
                        tracing::info!("No conflicting session found - proceed as normal");
                        self.phase_up = PhaseUp::PrepareMainSession(reg);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    _ => Err(InternalError::UnexpectedPhase),
                }
            }
            InternalEvent::UnregisterWg(_) => Err(InternalError::UnexpectedEvent(Box::new(event))),
        }
    }

    fn act_event_down(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            InternalEvent::OpenSession(res) => {
                check_entry_node(&res);
                let listen_host_used = matches!(&res, Err(session::Error::ListenHostAlreadyUsed));
                if let PhaseDown::PrepareBridgeSession(registration) = self.phase_down.clone() {
                    if listen_host_used {
                        tracing::warn!("Listen host already used - trying to close existing session");
                        self.phase_down = PhaseDown::FixBridgeSession(registration);
                        self.backoff = BackoffState::Inactive;
                        return Ok(());
                    };
                    self.phase_down = PhaseDown::WgUnregistration(res?, registration);
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            InternalEvent::CloseSession(res) => {
                check_entry_node(&res);
                let session_closed = matches!(&res, Err(session::Error::SessionNotFound));
                if !session_closed {
                    res?;
                }
                match self.phase_down.clone() {
                    PhaseDown::CloseTunnel(_session, registration) => {
                        self.phase_down = PhaseDown::PrepareBridgeSession(registration);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    PhaseDown::CloseBridgeSession(_session) => {
                        self.phase_down = PhaseDown::Retired;
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    PhaseDown::FixBridgeSessionClosing(_session, registration) => {
                        self.phase_down = PhaseDown::PrepareBridgeSession(registration);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    _ => Err(InternalError::UnexpectedPhase),
                }
            }
            InternalEvent::UnregisterWg(res) => {
                if let PhaseDown::WgUnregistration(session, _registration) = self.phase_down.clone() {
                    check_tcp_session(&res, session.port);
                    let already_unregistered = matches!(&res, Err(gvpn_client::Error::RegistrationNotFound));
                    if !already_unregistered {
                        res?;
                    }
                    self.phase_down = PhaseDown::CloseBridgeSession(session);
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            InternalEvent::ListSessions(res) => {
                check_entry_node(&res);
                let sessions = res?;
                let open_session = sessions.iter().find(|s| self.entry_node.conflicts_listen_host(s));
                match (open_session, self.phase_down.clone()) {
                    (Some(session), PhaseDown::FixBridgeSession(reg)) => {
                        tracing::info!(%session, "Found conflicting session - closing");
                        self.phase_down = PhaseDown::FixBridgeSessionClosing(session.clone(), reg);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    (None, PhaseDown::FixBridgeSession(reg)) => {
                        tracing::info!("No conflicting session found - proceed as normal");
                        self.phase_down = PhaseDown::PrepareBridgeSession(reg);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    _ => Err(InternalError::UnexpectedPhase),
                }
            }
            InternalEvent::Ping(_) | InternalEvent::RegisterWg(_) | InternalEvent::WgOpenTunnel(_) => {
                Err(InternalError::UnexpectedEvent(Box::new(event)))
            }
        }
    }

    fn register_wg(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        let ri = gvpn_client::Input::new(&self.wg.key_pair.public_key, &self.entry_node.endpoint, session);
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        thread::spawn(move || {
            let res = gvpn_client::register(&client, &ri);
            _ = s.send(InternalEvent::RegisterWg(res));
        });
        r
    }

    fn open_session(&mut self, params: session::OpenSession) -> crossbeam_channel::Receiver<InternalEvent> {
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(session_backoff());
        }
        thread::spawn(move || {
            let res = Session::open(&client, &params);
            _ = s.send(InternalEvent::OpenSession(res));
        });
        r
    }

    fn immediate_ping(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        let dest = self.destination.clone();
        let opts = dest.ping_options.clone();
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(immediate_ping_backoff());
        }
        thread::spawn(move || {
            let res = monitor::ping(&opts);
            _ = s.send(InternalEvent::Ping(res));
        });
        r
    }

    fn delayed_ping(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        let dest = self.destination.clone();
        let range = dest.ping_interval.clone();
        let opts = dest.ping_options.clone();
        thread::spawn(move || {
            let mut rng = rand::rng();
            let delay = Duration::from_secs(rng.random_range(range) as u64);
            let after = crossbeam_channel::after(delay);
            crossbeam_channel::select! {
                recv(after) -> _ => {
                    let res = monitor::ping(&opts);
                    _ = s.send(InternalEvent::Ping(res));
                }
            }
        });
        r
    }

    fn list_sessions(&mut self, protocol: &Protocol) -> crossbeam_channel::Receiver<InternalEvent> {
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
        let params = gvpn_client::Input::new(&self.wg.key_pair.public_key, &self.entry_node.endpoint, session);
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        thread::spawn(move || {
            let res = gvpn_client::unregister(&client, &params);
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

    fn open_wg_session(
        &mut self,
        session: &Session,
        registration: &Registration,
    ) -> crossbeam_channel::Receiver<InternalEvent> {
        let session = session.clone();
        let registration = registration.clone();
        let entry_node = self.entry_node.clone();
        let wg = self.wg.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let endpoint = match entry_node.endpoint_with_port(session.port) {
                Ok(endpoint) => endpoint,
                Err(error) => {
                    _ = s.send(InternalEvent::WgOpenTunnel(WgOpenResult::EntryNode(error)));
                    return;
                }
            };

            // run wg-quick down once to ensure no dangling state
            _ = wg.close_session();

            // connect wireguard
            let interface_info = wg_tooling::InterfaceInfo {
                address: registration.address(),
                allowed_ips: wg.config.allowed_ips.clone(),
                listen_port: wg.config.listen_port,
            };
            let peer_info = wg_tooling::PeerInfo {
                public_key: registration.server_public_key(),
                endpoint,
            };

            match wg.connect_session(&interface_info, &peer_info) {
                Ok(()) => {
                    _ = s.send(InternalEvent::WgOpenTunnel(WgOpenResult::Ok));
                }
                Err(error) => {
                    _ = s.send(InternalEvent::WgOpenTunnel(WgOpenResult::WgTooling(error)));
                }
            }
        });
        r
    }

    fn close_wg_session(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        _ = self.wg.close_session().map_err(|error| {
            tracing::warn!(warn = %error, "Failed closing WireGuard tunnel");
        });
        self.close_session(session)
    }

    /// Final state before dropping
    fn shutdown(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        _ = self
            .sender
            .send(Event::Dismantled)
            .map_err(|error| tracing::error!(%error, "Failed sending dismantled event"));
        crossbeam_channel::never()
    }

    fn bridge_session_params(&self) -> session::OpenSession {
        session::OpenSession::bridge(
            self.entry_node.clone(),
            self.destination.address,
            self.destination.bridge.capabilities.clone(),
            self.destination.path.clone(),
            self.destination.bridge.target.clone(),
        )
    }

    fn ping_session_params(&self) -> session::OpenSession {
        session::OpenSession::ping(
            self.entry_node.clone(),
            self.destination.address,
            self.destination.wg.capabilities.clone(),
            self.destination.path.clone(),
            self.destination.wg.target.clone(),
        )
    }

    fn main_session_params(&self) -> session::OpenSession {
        session::OpenSession::main(
            self.entry_node.clone(),
            self.destination.address,
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
            PhaseUp::FixBridgeSessionClosing(session) => write!(f, "FixBridgeSessionClosing({session})"),
            PhaseUp::WgRegistration(session) => write!(f, "WgRegistration({session})"),
            PhaseUp::CloseBridgeSession(session, registration) => {
                write!(f, "CloseBridgeSession({session}, {registration})")
            }
            PhaseUp::PreparePingSession(registration) => write!(f, "PreparePingSession({registration})"),
            PhaseUp::FixPingSession(registration) => write!(f, "FixPingSession({registration})"),
            PhaseUp::FixPingSessionClosing(session, registration) => {
                write!(f, "FixPingSessionClosing({session}, {registration})")
            }
            PhaseUp::PreparePingTunnel(session, registration) => {
                write!(f, "PreparePingTunnel({session}, {registration})")
            }
            PhaseUp::CheckPingTunnel(session, registration) => {
                write!(f, "CheckPingTunnel({session}, {registration})")
            }
            PhaseUp::ClosePingTunnel(session, registration) => {
                write!(f, "ClosePingTunnel({session}, {registration})")
            }
            PhaseUp::PrepareMainSession(registration) => write!(f, "PrepareMainSession({registration})"),
            PhaseUp::FixMainSession(registration) => write!(f, "FixMainSession({registration})"),
            PhaseUp::FixMainSessionClosing(session, registration) => {
                write!(f, "FixMainSessionClosing({session}, {registration})")
            }
            PhaseUp::PrepareMainTunnel(session, registration, since) => write!(
                f,
                "PrepareMainTunnel({}, {}, since {})",
                session,
                registration,
                log_output::elapsed(since)
            ),
            PhaseUp::TunnelEstablished(session, registration, since) => write!(
                f,
                "TunnelEstablished({}, since {}, {})",
                session,
                log_output::elapsed(since),
                registration,
            ),
            PhaseUp::MonitorTunnel(session, registration, since) => write!(
                f,
                "MonitorTunnel({}, since {}, {})",
                session,
                log_output::elapsed(since),
                registration,
            ),
            PhaseUp::TunnelBroken(session, registration) => {
                write!(f, "TunnelBroken({session}, {registration})")
            }
        }
    }
}

impl Display for PhaseDown {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PhaseDown::CloseTunnel(session, registration) => write!(f, "CloseTunnel({}, {})", session, registration),
            PhaseDown::PrepareBridgeSession(registration) => write!(f, "PrepareBridgeSession({registration})"),
            PhaseDown::FixBridgeSession(registration) => write!(f, "FixBridgeSession({registration})"),
            PhaseDown::FixBridgeSessionClosing(session, registration) => {
                write!(f, "FixBridgeSessionClosing({session}, {registration})")
            }
            PhaseDown::WgUnregistration(session, registration) => {
                write!(f, "WgUnregistration({session}, {registration})")
            }
            PhaseDown::CloseBridgeSession(session) => write!(f, "CloseBridgeSession({session})"),
            PhaseDown::Retired => write!(f, "Retired"),
        }
    }
}

impl From<PhaseUp> for PhaseDown {
    fn from(phase_up: PhaseUp) -> Self {
        match phase_up {
            PhaseUp::Ready => PhaseDown::Retired,
            PhaseUp::FixBridgeSession => PhaseDown::Retired,
            PhaseUp::FixBridgeSessionClosing(session) => PhaseDown::CloseBridgeSession(session),
            PhaseUp::WgRegistration(session) => PhaseDown::CloseBridgeSession(session),
            PhaseUp::CloseBridgeSession(session, registration) => PhaseDown::WgUnregistration(session, registration),
            PhaseUp::PreparePingSession(registration) => PhaseDown::PrepareBridgeSession(registration),
            PhaseUp::FixPingSession(registration) => PhaseDown::PrepareBridgeSession(registration),
            PhaseUp::FixPingSessionClosing(session, registration) => PhaseDown::CloseTunnel(session, registration),
            PhaseUp::PreparePingTunnel(session, registration) => PhaseDown::CloseTunnel(session, registration),
            PhaseUp::CheckPingTunnel(session, registration) => PhaseDown::CloseTunnel(session, registration),
            PhaseUp::ClosePingTunnel(session, registration) => PhaseDown::CloseTunnel(session, registration),
            PhaseUp::PrepareMainSession(registration) => PhaseDown::PrepareBridgeSession(registration),
            PhaseUp::FixMainSession(registration) => PhaseDown::PrepareBridgeSession(registration),
            PhaseUp::FixMainSessionClosing(session, registration) => PhaseDown::CloseTunnel(session, registration),
            PhaseUp::PrepareMainTunnel(session, registration, since) => PhaseDown::CloseTunnel(session, registration),
            PhaseUp::TunnelEstablished(session, registration, since) => PhaseDown::CloseTunnel(session, registration),
            PhaseUp::MonitorTunnel(session, registration, since) => PhaseDown::CloseTunnel(session, registration),
            PhaseUp::TunnelBroken(session, registration) => PhaseDown::CloseTunnel(session, registration),
        }
    }
}

impl Display for InternalEvent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            InternalEvent::OpenSession(res) => write!(f, "OpenSession({res:?})"),
            InternalEvent::CloseSession(res) => write!(f, "CloseSession({res:?})"),
            InternalEvent::RegisterWg(res) => write!(f, "RegisterWg({res:?})"),
            InternalEvent::UnregisterWg(res) => write!(f, "UnregisterWg({res:?})"),
            InternalEvent::Ping(res) => write!(f, "Ping({res:?})"),
            InternalEvent::ListSessions(res) => write!(f, "ListSessions({res:?})"),
            InternalEvent::WgOpenTunnel(res) => write!(f, "WgOpenTunnel({res:?})"),
        }
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::Connected => write!(f, "Connected"),
            Event::Disconnected => write!(f, "Disconnected"),
            Event::Broken => write!(f, "Broken"),
            Event::Dismantled => write!(f, "Dismantled"),
        }
    }
}

fn check_tcp_session<R>(res: &Result<R, gvpn_client::Error>, port: u16) {
    match res {
        Err(gvpn_client::Error::SocketConnect(_)) => log_output::print_port_instructions(port, Protocol::Tcp),
        Err(gvpn_client::Error::ConnectionReset(_)) => log_output::print_session_path_instructions(),
        _ => (),
    }
}

fn check_entry_node<R>(res: &Result<R, session::Error>) {
    match res {
        Err(session::Error::Unauthorized) => log_output::print_node_access_instructions(),
        Err(session::Error::SocketConnect(_)) => log_output::print_node_port_instructions(),
        Err(session::Error::Timeout(_)) => log_output::print_node_timeout_instructions(),
        _ => (),
    }
}

fn immediate_ping_backoff() -> ExponentialBackoff {
    ExponentialBackoffBuilder::new()
        .with_initial_interval(Duration::from_millis(30))
        .with_randomization_factor(0.3)
        .with_multiplier(1.1)
        .with_max_elapsed_time(Some(Duration::from_secs(3)))
        .build()
}

fn session_backoff() -> ExponentialBackoff {
    ExponentialBackoffBuilder::new()
        .with_initial_interval(Duration::from_secs(1))
        .with_randomization_factor(0.2)
        .with_multiplier(1.5)
        .with_max_elapsed_time(Some(Duration::from_secs(10)))
        .build()
}
