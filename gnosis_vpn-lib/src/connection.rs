use crossbeam_channel;
use reqwest::blocking;
use std::thread;

use crate::entry_node::EntryNode;
use crate::session;
use crate::wg_client;

/// Represents the different phases of a connection
/// Up: Idle -> SetUpBridgeSession -> RegisterWg -> TearDownBridgeSession -> SetUpMainSession -> ConnectWg -> Ready
/// Down: Ready -> DisconnectWg -> TearDownBridgeSession -> SetUpBridgeSession -> UnregisterWg -> TearDownBridgeSession -> Idle
#[derive(Clone, Debug)]
enum Phase {
    Idle,
    SetUpBridgeSession,
    TearDownBridgeSession,
    RegisterWg,
    UnregisterWg,
    SetUpMainSession,
    TearDownMainSession,
    ConnectWg,
    DisconnectWg,
    Ready,
}

#[derive(Clone, Debug)]
enum Direction {
    Up,
    Down,
    Halt,
}

#[derive(Debug)]
enum Event {
    Session(Result<session::Session, session::Error>),
    Register(Result<wg_client::Register, wg_client::Error>),
}

#[derive(Clone, Debug)]
pub struct Connection {
    phase: Phase,
    direction: Direction,
    // runtime data
    client: blocking::Client,
    abort_sender: Option<crossbeam_channel::Sender<()>>,
    // input data
    entry_node: EntryNode,
    destination: String,
    path: session::Path,
    target_bridge: session::Target,
    target_wg: session::Target,
    wg_public_key: String,
    // state data
    session: Option<session::Session>,
    wg_registration: Option<wg_client::Register>,
}

#[derive(Debug)]
enum Error {
    SessionNotSet,
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
            session: None,
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
            _ => {
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
            Event::Session(res) => match res {
                Ok(session) => {
                    self.session = Some(session);
                }
                Err(error) => {
                    tracing::error!(?error, "Failed to open session");
                    self.phase = Phase::Idle;
                }
            },
            Event::Register(res) => match res {
                Ok(register) => {
                    self.wg_registration = Some(register);
                }
                Err(error) => {
                    tracing::error!(?error, "Failed to register wg client");
                    self.phase = Phase::SetUpBridgeSession;
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
        let os = session::OpenSession::bridge(&self.entry_node, &self.destination, &self.path, &self.target_bridge);
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = session::open(&client, &os);
            _ = s.send(Event::Session(res));
        });
        Ok(r)
    }

    /// Transition from SetUpBridgeSession to RegisterWg
    fn bridge2wg(&mut self) -> Result<crossbeam_channel::Receiver<Event>, Error> {
        let session = self.session.as_ref().ok_or(Error::SessionNotSet)?;
        self.phase = Phase::RegisterWg;
        let ri = wg_client::RegisterInput::new(&self.wg_public_key, &self.entry_node.endpoint, &session);
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = wg_client::register(&client, &ri);
            _ = s.send(Event::Register(res));
        });
        Ok(r)
    }
}
