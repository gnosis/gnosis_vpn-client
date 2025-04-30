use crossbeam_channel;
use std::thread;

use crate::entry_node::EntryNode;
use crate::session;

/// Represents the different phases of a connection
/// Up: Idle -> SetUpBridgeSession -> RegisterWg -> TearDownBridgeSession -> SetUpMainSession -> ConnectWg -> Ready
/// Down: Ready -> DisconnectWg -> TearDownBridgeSession -> SetUpBridgeSession -> UnregisterWg -> TearDownBridgeSession -> Idle
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

enum Direction {
    Up,
    Down,
    Halt,
}

pub struct Connection {
    phase: Phase,
    direction: Direction,
    abort_receiver: crossbeam_channel::Receiver<()>,
    // input data
    entry_node: EntryNode,
    destination: String,
    path: Option<session::Path>,
    target: Option<session::Target>,
}

impl Connection {
    pub fn start(&self) {
        thread::spawn(move || loop {
            let receiver = self.act();
            crossbeam_channel::select! {
                recv(self.abort_receiver) -> _ => {
                    panic!("Do abort stuff");
                },
                recv(receiver) -> _ => {
                    panic!("Do receive stuff");
                }
            }
        });
    }
    pub fn act(&self) {
        match self.direction {
            Direction::Up => self.act_up(),
            Direction::Down => self.act_down(),
            Direction::Halt => self.act_halt(),
        }
    }

    pub fn abort(&self) {
        match self.phase {
            Phase::Idle => {}
            _ => {
                panic!("Invalid phase for abort action");
            }
        }
    }

    fn act_up(&self) {
        match self.phase {
            Phase::Idle => {
                self.idle2bridge();
            }
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

    fn act_halt(&self) {}

    /// Transition from Idle to SetUpBridgeSession
    fn idle2bridge(&self) {
        self.phase = Phase::SetUpBridgeSession;
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let os = session::OpenSession::bridge(&self.entry_node, &self.destination, &self.path, &self.target);
            let res = session::open(&os);
            s.send(res);
        });
    }
}
