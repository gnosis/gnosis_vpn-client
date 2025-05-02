use crossbeam_channel;
use reqwest::blocking;
use std::thread;

use crate::entry_node::EntryNode;
use crate::session;

/// Represents the different phases of a connection
/// Up: Idle -> SetUpBridgeSession -> RegisterWg -> TearDownBridgeSession -> SetUpMainSession -> ConnectWg -> Ready
/// Down: Ready -> DisconnectWg -> TearDownBridgeSession -> SetUpBridgeSession -> UnregisterWg -> TearDownBridgeSession -> Idle
#[derive(Clone)]
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

#[derive(Clone)]
enum Direction {
    Up,
    Down,
    Halt,
}

enum Event {
    Session(Result<session::Session, session::Error>),
}

#[derive(Clone)]
pub struct Connection {
    phase: Phase,
    direction: Direction,
    // runtime data
    client: blocking::Client,
    abort_sender: crossbeam_channel::Sender<()>,
    // input data
    entry_node: EntryNode,
    destination: String,
    path: Option<session::Path>,
    target: Option<session::Target>,
    // state data
    session: Option<session::Session>,
}

impl Connection {
    pub fn start(&mut self) {
        let (send_abort, recv_abort) = crossbeam_channel::bounded(1);
        self.abort_sender = send_abort;
        let mut me = self.clone();
        thread::spawn(move || loop {
            let recv_event: crossbeam_channel::Receiver<Event> = me.act_up();
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
        self.abort_sender.send(())
    }

    fn act_up(&mut self) -> crossbeam_channel::Receiver<Event> {
        self.direction = Direction::Up;
        match self.phase {
            Phase::Idle => self.idle2bridge(),
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
                    self.phase = Phase::Ready;
                }
                Err(error) => {
                    tracing::error!(?error, "Failed to open session");
                    self.phase = Phase::Idle;
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
    fn idle2bridge(&mut self) -> crossbeam_channel::Receiver<Event> {
        self.phase = Phase::SetUpBridgeSession;
        let entry_node = self.entry_node.clone();
        let destination = self.destination.clone();
        let path = self.path.clone();
        let target = self.target.clone();
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let os = session::OpenSession::bridge(&entry_node, &destination, &path, &target);
            let res = session::open(&client, &os);
            _ = s.send(Event::Session(res));
        });
        r
    }

    /// Transition from SetUpBridgeSession to RegisterWg
    fn bridge2wg(&mut self) -> crossbeam_channel::Receiver<Event> {
        self.phase = Phase::RegisterWg;
        let (_s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {});
        r
    }
}
