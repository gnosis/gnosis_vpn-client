use gnosis_vpn_lib::command::Command;
use gnosis_vpn_lib::config::Config;
use gnosis_vpn_lib::hopr_params::HoprParams;
use gnosis_vpn_lib::worker_command::WorkerCommand;
use gnosis_vpn_lib::event;

#[derive(Debug, Clone)]
pub struct Init {
    state: State,
}

#[derive(Debug, Clone)]
enum State {
    AwaitingResources,
    AwaitingHoprParams(Config),
    AwaitingConfig(HoprParams),
    Ready(Config, HoprParams),
    CoreRunning,
    Shutdown,
}

impl Init {
    pub fn new() -> Self {
        Init {
            state: State::AwaitingResources,
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.state, State::Ready(_, _))
    }

    pub fn is_shutdown(&self) -> bool {
        matches!(self.state, State::Shutdown)
    }

    pub fn incoming_cmd(&mut self, cmd: WorkerCommand) -> Option<Command> {
        match (self.state.clone(), cmd) {
            (State::CoreRunning, WorkerCommand::Shutdown) => {
                self.state = State::Shutdown;
                Some(Command::Shutdown)
            }
            (_, WorkerCommand::Shutdown) => {
                self.state = State::Shutdown;
                None,
            }
            (State::AwaitingResources, WorkerCommand::HoprParams { hopr_params }) => {
                self.state = State::AwaitingConfig(hopr_params);
                None
            }
            (State::AwaitingResources, WorkerCommand::Config { config }) => {
                self.state = State::AwaitingHoprParams(config);
                None
            }
            (State::AwaitingHoprParams(config), WorkerCommand::HoprParams { hopr_params }) => {
                self.state = State::Ready(config, hopr_params);
                None
            }
            (State::AwaitingConfig(hopr_params), WorkerCommand::Config { config }) => {
                self.state = State::Ready(config, hopr_params);
                None
            }
            (State::Ready(_, _), WorkerCommand::Command { cmd }) => Some(cmd),
            (state, worker_command) => {
                tracing::warn!(?state, ?worker_command, "received unexpected worker command");
                None
            }
        }
    }
}
