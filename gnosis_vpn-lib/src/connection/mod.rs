use backoff::{ExponentialBackoff, ExponentialBackoffBuilder, backoff::Backoff};
use crossbeam_channel;
use edgli::hopr_lib::IpProtocol;
use rand::Rng;
use reqwest::blocking;
use thiserror::Error;

use std::fmt::{self, Display};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::gvpn_client::{self, Registration};
use crate::hopr::{Hopr, HoprError};
use crate::log_output;
use crate::ping;
use crate::session::{self, Protocol, Session, to_surb_balancer_config};
use crate::wg_tooling;

use destination::Destination;
use monitor::Monitor;
use options::Options;

pub mod destination;
mod monitor;
pub mod options;

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

/// Represents the different phases of establishing a connection.
#[derive(Clone, Debug)]
enum PhaseUp {
    Ready,
    WgRegistration(Session),
    CloseBridgeSession(Session, Registration),
    PreparePingSession(Registration),
    PreparePingTunnel(Session, Registration),
    CheckPingTunnel(Session, Registration),
    UpgradeToMainTunnel(Session, Registration),
    MonitorTunnel(Session, Registration, Monitor),
    TunnelBroken(Session, Registration),
}

/// Represents the different phases of dismantling a connection.
#[derive(Clone, Debug)]
enum PhaseDown {
    CloseTunnel(Session, Registration),
    PrepareBridgeSession(Registration),
    WgUnregistration(Session, Registration),
    CloseBridgeSession(Session),
    Retired,
}

#[derive(Debug)]
enum InternalEvent {
    OpenSession(Result<Session, HoprError>),
    UpdateSession(Result<(), HoprError>),
    CloseSession(Result<(), HoprError>),
    ListSessions(Result<Vec<Session>, HoprError>),
    RegisterWg(Result<Registration, gvpn_client::Error>),
    UnregisterWg(Result<(), gvpn_client::Error>),
    WgOpenTunnel(WgOpenResult),
    Ping(Result<(), ping::Error>),
}

#[derive(Debug)]
enum WgOpenResult {
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

#[derive(Debug, Error)]
enum InternalError {
    #[error("Invalid phase for action")]
    UnexpectedPhase,
    #[error("External session error: {0}")]
    SessionError(#[from] HoprError),
    #[error("External Gnosis VPN error: {0}")]
    WgError(#[from] gvpn_client::Error),
    #[error("Channel send error: {0}")]
    SendError(#[from] crossbeam_channel::SendError<Event>),
    #[error("WireGuard error: {0}")]
    WireGuard(#[from] wg_tooling::Error),
    #[error("Unexpected event: {0}")]
    UnexpectedEvent(Box<InternalEvent>),
}

#[derive(Clone)]
pub struct Connection {
    // message passing helper
    establish_channel: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),
    dismantle_channel: (crossbeam_channel::Sender<PhaseUp>, crossbeam_channel::Receiver<PhaseUp>),

    // reuse http client
    client: blocking::Client,
    // hopr client
    edgli: Arc<Hopr>,

    // dynamic runtime data
    phase_up: PhaseUp,
    phase_down: PhaseDown,
    backoff: BackoffState,

    // static input data
    destination: Destination,
    wg: wg_tooling::WireGuard,
    sender: crossbeam_channel::Sender<Event>,
    options: Options,
}

impl Connection {
    pub fn new(
        edgli: Arc<Hopr>,
        destination: Destination,
        wg: wg_tooling::WireGuard,
        sender: crossbeam_channel::Sender<Event>,
        options: Options,
    ) -> Self {
        Connection {
            destination,
            edgli,
            sender,
            wg,
            backoff: BackoffState::Inactive,
            client: blocking::Client::new(),
            dismantle_channel: crossbeam_channel::bounded(1),
            establish_channel: crossbeam_channel::bounded(1),
            phase_down: PhaseDown::Retired,
            phase_up: PhaseUp::Ready,
            options,
        }
    }

    pub fn has_destination(&self, destination: &Destination) -> bool {
        self.destination.address == destination.address
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
                            tracing::debug!(phase = "up", ?backoff, delay = ?delay, "Triggering backoff delay");
                            me.backoff = BackoffState::Triggered(backoff);
                            (crossbeam_channel::never(), crossbeam_channel::after(delay))
                        }
                        None => {
                            me.backoff = BackoffState::Inactive;
                            tracing::error!(phase = "up", "Unrecoverable error: backoff exhausted");
                            _ = me.sender.send(Event::Broken).map_err(|error| {
                                tracing::error!(%error, "Failed sending broken event");
                            });
                            (crossbeam_channel::never(), crossbeam_channel::never())
                        }
                    },
                    BackoffState::Triggered(backoff) => {
                        tracing::debug!(phase = "up", ?backoff, "Activating backoff");
                        me.backoff = BackoffState::Active(backoff);
                        (me.act_up(), crossbeam_channel::never())
                    }
                    BackoffState::NotRecoverable(error) => {
                        tracing::error!(phase = "up", %error, "Unrecoverable error: connection broken");
                        _ = me.sender.send(Event::Broken).map_err(|error| {
                            tracing::error!(phase = "up", %error, "Failed sending broken event");
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
                                        tracing::error!(phase = "up", %error, "Unrecoverable error: sending connection data on dismantle channel");
                                        _ = me.sender.send(Event::Dismantled).map_err(|error| {
                                            tracing::error!(%error, "Failed sending dismantled event");
                                        });
                                    }
                                }
                                break;
                            }
                            Err(error) => {
                                tracing::error!(phase = "up", %error, "Failed receiving signal on establish channel");
                            }
                        }
                    },
                    recv(recv_backoff) -> _ => {
                        tracing::debug!(phase = "up", "Backoff delay hit - loop to act");
                    },
                    recv(recv_event) -> res => {
                        match res {
                            Ok(event) => {
                                tracing::debug!(phase = "up", %event, "Received event");
                                _ = me.act_event_up(event).map_err(|error| {
                                    tracing::error!(phase = "up", %error, "Failed to process event");
                                });
                            }
                            Err(error) => {
                                tracing::error!(phase = "up", %error, "Failed receiving event");
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
                    tracing::error!(phase = "down", %error, "Unrecoverable error: sending dismantle signal on establish channel");
                    return;
                }
            }
            outer.phase_up = crossbeam_channel::select! {
                recv(outer.dismantle_channel.1) -> res => {
                    match res {
                        Ok(data) => data,
                        Err(error) => {
                            tracing::error!(phase = "down", %error, "Unrecoverable error: receiving runtime data on dismantle channel");
                            return;
                        }
                    }
                }
                default(Duration::from_secs(5)) => {
                            tracing::error!(phase = "down", "Unrecoverable error: timeout receiving connection data on dismantle channel");
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
                                tracing::debug!(phase = "down", ?backoff, delay = ?delay, "Triggering backoff delay");
                                me.backoff = BackoffState::Triggered(backoff);
                                (crossbeam_channel::never(), crossbeam_channel::after(delay))
                            }
                            None => {
                                me.backoff = BackoffState::Inactive;
                                tracing::error!(phase = "down", "Unrecoverable error: backoff exhausted");
                                _ = me.sender.send(Event::Dismantled).map_err(|error| {
                                    tracing::error!(phase = "down", %error, "Failed sending dismantled event");
                                });
                                break;
                            }
                        },
                        BackoffState::Triggered(backoff) => {
                            tracing::debug!(phase = "down", ?backoff, "Activating backoff");
                            me.backoff = BackoffState::Active(backoff);
                            (me.act_down(), crossbeam_channel::never())
                        }
                        BackoffState::NotRecoverable(error) => {
                            tracing::error!(phase = "down", %error, "Unrecoverable error: connection broken");
                            _ = me.sender.send(Event::Dismantled).map_err(|error| {
                                tracing::error!(phase = "down", %error, "Failed sending dismantled event");
                            });
                            break;
                        }
                    };
                    // main listening loop
                    crossbeam_channel::select! {
                        recv(recv_backoff) -> _ => {
                            tracing::debug!(phase = "up", "Backoff delay hit - loop to act");
                        }
                        recv(recv_event) -> res => {
                            match res {
                                Ok(evt) => {
                                    tracing::debug!(phase = "up", event = ?evt, "Received event");
                                    _ = me.act_event_down(evt).map_err(|error| {
                                        tracing::error!(phase = "up", %error, "Failed to process event");
                                    });
                                }
                                Err(error) => {
                                    tracing::error!(phase = "up", %error, "Failed receiving event");
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
            PhaseUp::WgRegistration(session) => self.register_wg(&session),
            PhaseUp::CloseBridgeSession(session, _registration) => self.close_session(&session),
            PhaseUp::PreparePingSession(_registration) => self.open_session(self.ping_session_params()),
            PhaseUp::PreparePingTunnel(session, registration) => self.open_wg_session(&session, &registration),
            PhaseUp::CheckPingTunnel(_session, _registration) => self.immediate_ping(),
            PhaseUp::UpgradeToMainTunnel(session, _registration) => self.set_main_config(&session),
            PhaseUp::MonitorTunnel(_session, _registration, _monitor) => self.list_tcp_sessions_delayed(),
            PhaseUp::TunnelBroken(session, _registration) => self.close_wg_session(&session),
        }
    }

    fn act_down(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        tracing::debug!(phase_down = %self.phase_down, "Dismantling connection");
        match self.phase_down.clone() {
            PhaseDown::CloseTunnel(session, _registration) => self.close_wg_session(&session),
            PhaseDown::PrepareBridgeSession(_registration) => self.open_session(self.bridge_session_params()),
            PhaseDown::WgUnregistration(session, _registration) => self.unregister_wg(&session),
            PhaseDown::CloseBridgeSession(session) => self.close_session(&session),
            PhaseDown::Retired => self.shutdown(),
        }
    }

    fn act_event_up(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            // handle open session event depending on phase
            InternalEvent::OpenSession(res) => match self.phase_up.clone() {
                PhaseUp::Ready => {
                    self.phase_up = PhaseUp::WgRegistration(res?.clone());
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                }
                PhaseUp::PreparePingSession(registration) => {
                    let session = res?;
                    self.phase_up = PhaseUp::PreparePingTunnel(session, registration);
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                }
                _ => Err(InternalError::UnexpectedPhase),
            },

            // handle wg open tunnel event depending on result and phase
            InternalEvent::WgOpenTunnel(res) => match (res, self.phase_up.clone()) {
                (WgOpenResult::WgTooling(error), _) => {
                    self.backoff = BackoffState::NotRecoverable(format!("{error}"));
                    Ok(())
                }
                (WgOpenResult::Ok, PhaseUp::PreparePingTunnel(session, registration)) => {
                    self.phase_up = PhaseUp::CheckPingTunnel(session.clone(), registration.clone());
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                }
                (_, _) => Err(InternalError::UnexpectedPhase),
            },

            // handle wg registration event depending on phase
            InternalEvent::RegisterWg(res) => {
                if let PhaseUp::WgRegistration(session) = self.phase_up.clone() {
                    check_tcp_session(&res, session.bound_host.port());
                    self.phase_up = PhaseUp::CloseBridgeSession(session, res?);
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }

            // handle close session event depending on phase
            InternalEvent::CloseSession(res) => {
                let session_closed = matches!(&res, Err(HoprError::SessionNotFound));
                if !session_closed {
                    res?;
                }
                match self.phase_up.clone() {
                    PhaseUp::CloseBridgeSession(_session, registration) => {
                        self.phase_up = PhaseUp::PreparePingSession(registration);
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
                    self.phase_up = PhaseUp::UpgradeToMainTunnel(session, registration);
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                }
                (Err(error), PhaseUp::CheckPingTunnel(session, _registration)) => {
                    tracing::warn!(%session, %error, "Ping during initial check failed");
                    if !error.would_block() {
                        log_output::print_port_instructions(session.bound_host.port(), Protocol::Udp);
                    }
                    Ok(())
                }
                _ => Err(InternalError::UnexpectedPhase),
            },

            InternalEvent::ListSessions(res) => match (res?, self.phase_up.clone()) {
                (sessions, PhaseUp::MonitorTunnel(session, registration, monitor)) => {
                    if session.verify_open(&sessions) {
                        tracing::info!(%session, "Session existence verified for {}", log_output::elapsed(&monitor.since));
                        self.phase_up = PhaseUp::MonitorTunnel(session, registration, monitor.reset_success());
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    } else {
                        tracing::warn!(%session, "Session not found in active sessions");
                        self.phase_up = PhaseUp::TunnelBroken(session, registration);
                        self.backoff = BackoffState::Inactive;
                        self.sender.send(Event::Disconnected).map_err(InternalError::SendError)
                    }
                }
                _ => Err(InternalError::UnexpectedPhase),
            },

            InternalEvent::UpdateSession(res) => {
                res?;
                match self.phase_up.clone() {
                    PhaseUp::UpgradeToMainTunnel(session, registration) => {
                        tracing::info!(%session, "Session upgraded to main tunnel");
                        log_output::print_session_established(&self.pretty_print_path());
                        self.phase_up = PhaseUp::MonitorTunnel(session, registration, Monitor::new());
                        self.backoff = BackoffState::Inactive;
                        self.sender.send(Event::Connected).map_err(InternalError::SendError)
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
                if let PhaseDown::PrepareBridgeSession(registration) = self.phase_down.clone() {
                    self.phase_down = PhaseDown::WgUnregistration(res?, registration);
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                } else {
                    Err(InternalError::UnexpectedPhase)
                }
            }
            InternalEvent::CloseSession(res) => {
                let session_closed = matches!(&res, Err(HoprError::SessionNotFound));
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
                    _ => Err(InternalError::UnexpectedPhase),
                }
            }
            InternalEvent::UnregisterWg(res) => {
                if let PhaseDown::WgUnregistration(session, _registration) = self.phase_down.clone() {
                    check_tcp_session(&res, session.bound_host.port());
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
            InternalEvent::Ping(_)
            | InternalEvent::ListSessions(_)
            | InternalEvent::RegisterWg(_)
            | InternalEvent::WgOpenTunnel(_)
            | InternalEvent::UpdateSession(_) => Err(InternalError::UnexpectedEvent(Box::new(event))),
        }
    }

    fn register_wg(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        unimplemented!();
        /*
        let ri = gvpn_client::Input::new(
            self.wg.key_pair.public_key.clone(),
            session.clone(),
            self.options.timeouts.http,
        );
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
        */
    }

    fn open_session(&mut self, params: session::OpenSession) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        thread::spawn(move || {
            let res = Session::open(&params);
            // _ = s.send(InternalEvent::OpenSession(res));
        });
        r
    }

    fn list_tcp_sessions_delayed(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        let edgli = self.edgli.clone();
        thread::spawn(move || {
            let mut rng = rand::rng();
            let delay = Duration::from_secs(rng.random_range(1..10) as u64);
            let after = crossbeam_channel::after(delay);
            crossbeam_channel::select! {
                recv(after) -> _ => {
                    let params = session::ListSession::new(edgli, IpProtocol::UDP);
                    let res = Session::list(&params);
                    _ = s.send(InternalEvent::ListSessions(res));
                }
            }
        });
        r
    }

    fn immediate_ping(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        let opts = self.options.ping_options.clone();
        let timeout = self.options.timeouts.ping_retries;
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(immediate_ping_backoff(timeout));
        }
        thread::spawn(move || {
            let res = ping::ping(&opts);
            _ = s.send(InternalEvent::Ping(res));
        });
        r
    }

    fn unregister_wg(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        unimplemented!();
        /*
        let params = gvpn_client::Input::new(
            self.wg.key_pair.public_key.clone(),
            session.clone(),
            self.options.timeouts.http,
        );
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
        */
    }

    fn close_session(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        let params = session::CloseSession::new(self.edgli.clone());
        let (s, r) = crossbeam_channel::bounded(1);
        let session = session.clone();
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        thread::spawn(move || {
            let res = session.close(&params);
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
        let wg = self.wg.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            // run wg-quick down once to ensure no dangling state
            _ = wg.close_session();

            // connect wireguard
            let interface_info = wg_tooling::InterfaceInfo {
                address: registration.address(),
                allowed_ips: wg.config.allowed_ips.clone(),
                listen_port: wg.config.listen_port,
                mtu: session.hopr_mtu,
            };
            let peer_info = wg_tooling::PeerInfo {
                public_key: registration.server_public_key(),
                endpoint: format!("127.0.0.1:{}", session.bound_host.port()),
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

    fn set_main_config(&mut self, session: &Session) -> crossbeam_channel::Receiver<InternalEvent> {
        let params = session::UpdateSessionConfig::new(
            self.edgli.clone(),
            self.options.buffer_sizes.main,
            self.options.max_surb_upstream.main,
        );
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        let session = session.clone();
        thread::spawn(move || {
            let res = session.update(&params);
            if let Err(error) = s.send(InternalEvent::UpdateSession(res)) {
                tracing::error!(%error, "Failed sending update session event");
            }
        });
        r
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
            self.edgli.clone(),
            self.destination.address,
            self.options.sessions.bridge.capabilities,
            self.destination.routing.clone(),
            self.options.sessions.bridge.target.clone(),
            to_surb_balancer_config(self.options.buffer_sizes.bridge, self.options.max_surb_upstream.bridge),
        )
    }

    fn ping_session_params(&self) -> session::OpenSession {
        session::OpenSession::main(
            self.edgli.clone(),
            self.destination.address,
            self.options.sessions.wg.capabilities,
            self.destination.routing.clone(),
            self.options.sessions.wg.target.clone(),
            to_surb_balancer_config(self.options.buffer_sizes.ping, self.options.max_surb_upstream.ping),
        )
    }
}

impl Display for PhaseUp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PhaseUp::Ready => write!(f, "Ready"),
            PhaseUp::WgRegistration(session) => write!(f, "WgRegistration({session})"),
            PhaseUp::CloseBridgeSession(session, registration) => {
                write!(f, "CloseBridgeSession({session}, {registration})")
            }
            PhaseUp::PreparePingSession(registration) => write!(f, "PreparePingSession({registration})"),
            PhaseUp::PreparePingTunnel(session, registration) => {
                write!(f, "PreparePingTunnel({session}, {registration})")
            }
            PhaseUp::CheckPingTunnel(session, registration) => {
                write!(f, "CheckPingTunnel({session}, {registration})")
            }
            PhaseUp::UpgradeToMainTunnel(session, registration) => {
                write!(f, "UpgradeToMainTunnel({session}, {registration})")
            }
            PhaseUp::MonitorTunnel(session, registration, monitor) => {
                write!(f, "MonitorTunnel({session}, {monitor}, {registration})",)
            }
            PhaseUp::TunnelBroken(session, registration) => {
                write!(f, "TunnelBroken({session}, {registration})")
            }
        }
    }
}

impl Display for PhaseDown {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PhaseDown::CloseTunnel(session, registration) => write!(f, "CloseTunnel({session}, {registration})"),
            PhaseDown::PrepareBridgeSession(registration) => write!(f, "PrepareBridgeSession({registration})"),
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
            PhaseUp::WgRegistration(session) => PhaseDown::CloseBridgeSession(session),
            PhaseUp::CloseBridgeSession(session, registration) => PhaseDown::WgUnregistration(session, registration),
            PhaseUp::PreparePingSession(registration) => PhaseDown::PrepareBridgeSession(registration),
            PhaseUp::PreparePingTunnel(session, registration) => PhaseDown::CloseTunnel(session, registration),
            PhaseUp::CheckPingTunnel(session, registration) => PhaseDown::CloseTunnel(session, registration),
            PhaseUp::UpgradeToMainTunnel(session, registration) => PhaseDown::CloseTunnel(session, registration),
            PhaseUp::MonitorTunnel(session, registration, _since) => PhaseDown::CloseTunnel(session, registration),
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
            InternalEvent::WgOpenTunnel(res) => write!(f, "WgOpenTunnel({res:?})"),
            InternalEvent::UpdateSession(res) => write!(f, "UpdateSession({res:?})"),
            InternalEvent::ListSessions(res) => write!(f, "ListSessions({res:?})"),
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

fn immediate_ping_backoff(timeout: Duration) -> ExponentialBackoff {
    ExponentialBackoffBuilder::new()
        .with_initial_interval(Duration::from_millis(30))
        .with_randomization_factor(0.3)
        .with_multiplier(1.1)
        .with_max_elapsed_time(Some(timeout))
        .build()
}
