use gnosis_vpn_lib::config::Config;
use gnosis_vpn_lib::event::RootToWorker;
use gnosis_vpn_lib::worker_params::WorkerParams;

#[derive(Debug, Clone)]
pub struct Init {
    state: State,
}

#[derive(Debug, Clone)]
enum State {
    AwaitingResources,
    AwaitingHoprParams(Config),
    AwaitingConfig(WorkerParams),
    Ready(Config, WorkerParams),
}

impl Init {
    pub fn new() -> Self {
        Init {
            state: State::AwaitingResources,
        }
    }

    pub fn ready(&self) -> Option<(Config, WorkerParams)> {
        if let State::Ready(config, worker_params) = &self.state {
            Some((config.clone(), worker_params.clone()))
        } else {
            None
        }
    }

    pub fn incoming_cmd(&self, cmd: RootToWorker) -> Self {
        match (self.state.clone(), cmd) {
            (State::AwaitingResources, RootToWorker::WorkerParams { worker_params }) => Init {
                state: State::AwaitingConfig(worker_params),
            },
            (State::AwaitingResources, RootToWorker::Config { config }) => Init {
                state: State::AwaitingHoprParams(config),
            },
            (State::AwaitingHoprParams(config), RootToWorker::WorkerParams { worker_params }) => Init {
                state: State::Ready(config, worker_params),
            },
            (State::AwaitingConfig(worker_params), RootToWorker::Config { config }) => Init {
                state: State::Ready(config, worker_params),
            },
            (state, worker_command) => {
                tracing::warn!(?state, ?worker_command, "received unexpected worker command - ignoring");
                Init { state }
            }
        }
    }
}
