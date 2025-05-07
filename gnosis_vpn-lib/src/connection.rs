use crossbeam_channel;
use rand::Rng;
use reqwest::{blocking, StatusCode};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::entry_node::EntryNode;
use crate::log_output;
use crate::remote_data;
use crate::session::{self, Session};
use crate::wg_client;

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
enum Event {
    BridgeSession(Result<Session, session::Error>),
    RegisterWg(Result<wg_client::Register, wg_client::Error>),
    TearDownBridgeSession(Result<(), session::Error>),
    MainSession(Result<Session, session::Error>),
    ListSessions(Result<Vec<Session>, session::Error>),
}

#[derive(Clone, Debug)]
pub struct Connection {
    phase: Phase,
    direction: Direction,
    // runtime data
    abort_sender: Option<crossbeam_channel::Sender<()>>,
    client: blocking::Client,
    // input data
    entry_node: EntryNode,
    destination: String,
    path: session::Path,
    target_bridge: session::Target,
    target_wg: session::Target,
    wg_public_key: String,
    // state data
    session_since: Option<(Session, SystemTime)>,
    wg_registration: Option<wg_client::Register>,
}

#[derive(Debug)]
enum Error {
    SessionNotSet,
    WgRegistrationNotSet,
}

impl Connection {
    pub fn new(
        entry_node: &EntryNode,
        destination: &str,
        path: &session::Path,
        target_bridge: &session::Target,
        target_wg: &session::Target,
        wg_public_key: &str,
    ) -> Self {
        Connection {
            phase: Phase::Idle,
            direction: Direction::Halt,
            client: blocking::Client::new(),
            abort_sender: None,
            entry_node: entry_node.clone(),
            destination: destination.to_string(),
            path: path.clone(),
            target_bridge: target_bridge.clone(),
            target_wg: target_wg.clone(),
            wg_public_key: wg_public_key.to_string(),
            session_since: None,
            wg_registration: None,
        }
    }

    pub fn start(&mut self) {
        let (send_abort, recv_abort) = crossbeam_channel::bounded(1);
        self.abort_sender = Some(send_abort);
        let mut me = self.clone();
        thread::spawn(move || loop {
            let result = me.act_up();
            let recv_event = match result {
                Ok(recv_event) => recv_event,
                Err(error) => {
                    tracing::error!(?error, "Failed to act up");
                    crossbeam_channel::never()
                }
            };
            crossbeam_channel::select! {
                recv(recv_abort) -> res => {
                    match res {
                        Ok(_) => {
                            me.act_abort();
                        }
                        Err(error) => {
                            tracing::error!(?error, "Failed receiving abort signal");
                        }
                    }
                },
                recv(recv_event) -> res => {
                    match res {
                        Ok(evt) => {
                                tracing::info!(event = ?evt, "Received event");
                                me.act_event(evt)
                        }
                        Err(error) => {
                            tracing::error!(?error, "Failed receiving event");
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

    fn act_up(&mut self) -> Result<crossbeam_channel::Receiver<Event>, Error> {
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

    fn act_event(&mut self, event: Event) {
        match event {
            Event::BridgeSession(res) => match res {
                Ok(session) => {
                    self.session_since = Some((session, SystemTime::now()));
                }
                // Some(Object {"status": String("LISTEN_HOST_ALREADY_USED")}) })))
                Err(session::Error::RemoteData(remote_data::CustomError {
                    status: Some(StatusCode::CONFLICT),
                    value: Some(json),
                    reqw_err: _,
                })) => {
                    if (json["status"] == "LISTEN_HOST_ALREADY_USED") {
                        // TODO hanlde dismantling on existing port
                    }
                    tracing::error!(?json, "Failed to open session - CONFLICT");
                    self.phase = Phase::Idle;
                }
                Err(error) => {
                    tracing::error!(?error, "Failed to open session");
                    self.phase = Phase::Idle;
                }
            },
            Event::RegisterWg(res) => match res {
                Ok(register) => {
                    self.wg_registration = Some(register);
                }
                Err(error) => {
                    tracing::error!(?error, "Failed to register wg client");
                    self.phase = Phase::SetUpBridgeSession;
                }
            },
            Event::TearDownBridgeSession(res) => match res {
                Ok(_) => {
                    self.session_since = None;
                }
                Err(error) => {
                    tracing::error!(?error, "Failed to tear down bridge session");
                    self.phase = Phase::RegisterWg;
                }
            },
            Event::MainSession(res) => match res {
                Ok(session) => {
                    self.session_since = Some((session, SystemTime::now()));
                }
                Err(error) => {
                    tracing::error!(?error, "Failed to open main session");
                    self.phase = Phase::TearDownBridgeSession;
                }
            },
            Event::ListSessions(res) => match res {
                Ok(sessions) => match self.session_since.as_ref() {
                    Some((session, since)) => {
                        if session.verify_open(&sessions) {
                            let msg = format!("session verified open since {}", log_output::elapsed(&since));
                            tracing::info!(?sessions, msg);
                        } else {
                            tracing::info!("Session is closed");
                            self.session_since = None;
                            self.phase = Phase::TearDownBridgeSession;
                        }
                    }
                    None => {
                        tracing::warn!("List session results received but no session to verify");
                    }
                },
                Err(error) => {
                    tracing::error!(?error, "Failed to list sessions");
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
    fn idle2bridge(&mut self) -> Result<crossbeam_channel::Receiver<Event>, Error> {
        self.phase = Phase::SetUpBridgeSession;
        let params = session::OpenSession::bridge(
            &self.entry_node,
            &self.destination,
            &self.path,
            &self.target_bridge,
            &Duration::from_secs(15),
        );
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = Session::open(&client, &params);
            _ = s.send(Event::BridgeSession(res));
        });
        Ok(r)
    }

    /// Transition from SetUpBridgeSession to RegisterWg
    fn bridge2wg(&mut self) -> Result<crossbeam_channel::Receiver<Event>, Error> {
        let (session, _) = self.session_since.as_ref().ok_or(Error::SessionNotSet)?;
        self.phase = Phase::RegisterWg;
        let ri = wg_client::RegisterInput::new(&self.wg_public_key, &self.entry_node.endpoint, &session);
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = wg_client::register(&client, &ri);
            _ = s.send(Event::RegisterWg(res));
        });
        Ok(r)
    }

    /// Transition from RegisterWg to TearDownBridgeSession
    fn wg2teardown(&mut self) -> Result<crossbeam_channel::Receiver<Event>, Error> {
        let (session, _) = self.session_since.as_ref().ok_or(Error::SessionNotSet)?.clone();
        self.wg_registration.as_ref().ok_or(Error::WgRegistrationNotSet)?;
        self.phase = Phase::TearDownBridgeSession;
        let params = session::CloseSession::new(&self.entry_node, &Duration::from_secs(15));
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = session.close(&client, &params);
            _ = s.send(Event::TearDownBridgeSession(res));
        });
        Ok(r)
    }

    /// Transition from TearDownBridgeSession to SetUpMainSession
    fn teardown2main(&mut self) -> Result<crossbeam_channel::Receiver<Event>, Error> {
        self.phase = Phase::SetUpMainSession;
        let params = session::OpenSession::main(
            &self.entry_node,
            &self.destination,
            &self.path,
            &self.target_wg,
            &Duration::from_secs(20),
        );
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = Session::open(&client, &params);
            _ = s.send(Event::MainSession(res));
        });
        Ok(r)
    }

    fn monitor(&mut self) -> Result<crossbeam_channel::Receiver<Event>, Error> {
        let (session, _) = self.session_since.as_ref().ok_or(Error::SessionNotSet)?;
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
                    _ = s.send(Event::ListSessions(res));
                }
            }
        });
        Ok(r)
    }
}
