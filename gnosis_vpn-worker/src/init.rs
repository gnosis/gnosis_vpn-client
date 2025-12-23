use gnosis_vpn_lib::config::Config;
use gnosis_vpn_lib::event::RootToWorker;
use gnosis_vpn_lib::hopr_params::HoprParams;

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
}

impl Init {
    pub fn new() -> Self {
        Init {
            state: State::AwaitingResources,
        }
    }

    pub fn ready(&self) -> Option<(Config, HoprParams)> {
        if let State::Ready(config, hopr_params) = &self.state {
            Some((config.clone(), hopr_params.clone()))
        } else {
            None
        }
    }

    pub fn incoming_cmd(&self, cmd: RootToWorker) -> Self {
        match (self.state.clone(), cmd) {
            (State::AwaitingResources, RootToWorker::HoprParams { hopr_params }) => Init {
                state: State::AwaitingConfig(hopr_params),
            },
            (State::AwaitingResources, RootToWorker::Config { config }) => Init {
                state: State::AwaitingHoprParams(config),
            },
            (State::AwaitingHoprParams(config), RootToWorker::HoprParams { hopr_params }) => Init {
                state: State::Ready(config, hopr_params),
            },
            (State::AwaitingConfig(hopr_params), RootToWorker::Config { config }) => Init {
                state: State::Ready(config, hopr_params),
            },
            (state, worker_command) => {
                tracing::warn!(?state, ?worker_command, "received unexpected worker command - ignoring");
                Init { state }
            }
        }
    }
}
