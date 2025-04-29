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
}

impl Connection {
    pub fn new() -> Self {
        Connection {
            phase: Phase::Idle,
            direction: Direction::Halt,
        }
    }

    pub fn act(&self) -> {
        switch self.direction {
            Direction::Up => self.act_up(),
            Direction::Down => self.act_down(),
            Direction::Halt => _,
        }
    }

    fn act_up(&self) -> {
        switch self.phase {
            Phase::Idle => {
                self.phase = Phase::SetUpBridgeSession,
            }
            _ => {
                panic!("Invalid phase for up action");
            }
        }
    }

    fn act_down(&self) -> {
        switch self.phase {
            Phase::Idle => {},
            _ => {
                panic!("Invalid phase for down action");
            }
        }
    }

    fn act_halt(&self) -> {}
}
