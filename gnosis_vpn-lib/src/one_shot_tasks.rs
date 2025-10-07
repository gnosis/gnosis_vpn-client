use backoff::{ExponentialBackoff, backoff::Backoff};
use crossbeam_channel;
use edgli::hopr_lib::exports::crypto::types::prelude::ChainKeypair;
use thiserror::Error;
use tokio::runtime::Runtime;
use url::Url;

use std::fmt::{self, Display};
use std::thread;

use crate::chain::client::GnosisRpcClient;
use crate::chain::contracts::NetworkSpecifications;
use crate::chain::errors::ChainError;
use crate::network::Network;
use crate::ticket_stats::{self, TicketStats};

#[derive(Clone, Debug)]
pub enum Event {
    TicketStats(TicketStats),
    BackoffExhausted,
}

/// Represents the different phases of establishing a connection.
#[derive(Clone, Debug)]
enum Phase {
    TicketStats,
    Done,
}

#[derive(Debug)]
enum InternalEvent {
    TicketStats(Result<TicketStats, ChainError>),
}

#[derive(Clone, Debug)]
enum BackoffState {
    Inactive,
    Active(ExponentialBackoff),
    Triggered(ExponentialBackoff),
}

#[derive(Debug, Error)]
enum InternalError {
    #[error(transparent)]
    Chain(#[from] ChainError),
    #[error(transparent)]
    TicketStats(#[from] ticket_stats::Error),
}

#[derive(Clone)]
pub struct OneShotTasks {
    // message passing helper
    cancel_channel: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),

    // dynamic runtime data
    backoff: BackoffState,
    phase: Phase,

    // static input data
    sender: crossbeam_channel::Sender<Event>,
    private_key: ChainKeypair,
    rpc_provider: Url,
    network_specs: NetworkSpecifications,
}

impl OneShotTasks {
    pub fn new(
        sender: crossbeam_channel::Sender<Event>,
        private_key: ChainKeypair,
        rpc_provider: Url,
        network: Network,
    ) -> Self {
        OneShotTasks {
            cancel_channel: crossbeam_channel::bounded(1),
            backoff: BackoffState::Inactive,
            phase: Phase::TicketStats,
            sender,
            private_key,
            rpc_provider,
            network_specs: NetworkSpecifications::from_network(&network),
        }
    }

    /// Query info once and continuously monitor balance
    pub fn run(&self) {
        let mut me = self.clone();
        thread::spawn(move || {
            loop {
                let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        tracing::error!(%error, "Failed creating tokio runtime");
                        continue;
                    }
                };
                // Backoff handling
                // Inactive - no backoff was set, act up
                // Active - backoff was set and can trigger, don't act until backoff delay
                // Triggered - backoff was triggered, time to act up again keeping backoff active
                let (recv_event, recv_backoff) = match me.backoff.clone() {
                    BackoffState::Inactive => (me.act(rt), crossbeam_channel::never()),
                    BackoffState::Active(mut backoff) => match backoff.next_backoff() {
                        Some(delay) => {
                            tracing::debug!(phase = %me.phase, ?backoff, delay = ?delay, "Triggering backoff delay");
                            me.backoff = BackoffState::Triggered(backoff);
                            (crossbeam_channel::never(), crossbeam_channel::after(delay))
                        }
                        None => {
                            me.backoff = BackoffState::Inactive;
                            tracing::error!(phase = %me.phase, "Unrecoverable error: backoff exhausted");
                            _ = me.sender.send(Event::BackoffExhausted).map_err(|error| {
                                tracing::error!(%error, "Failed sending exhausted event");
                            });
                            (crossbeam_channel::never(), crossbeam_channel::never())
                        }
                    },
                    BackoffState::Triggered(backoff) => {
                        tracing::debug!(phase = %me.phase, ?backoff, "Activating backoff");
                        me.backoff = BackoffState::Active(backoff);
                        (me.act(rt), crossbeam_channel::never())
                    }
                };

                crossbeam_channel::select! {
                    // checking on cancel signal
                    recv(me.cancel_channel.1) -> _ => break,
                    recv(recv_backoff) -> _ => {
                        tracing::debug!(phase = %me.phase, "Backoff delay hit - loop to act");
                    },
                    recv(recv_event) -> res => {
                        match res {
                            Ok(event) => {
                                tracing::debug!(phase = %me.phase, %event, "Received event");
                                _ = me.event(event).map_err(|error| {
                                    tracing::error!(phase = %me.phase, %error, "Failed to process event");
                                });
                            }
                            Err(error) => {
                                tracing::error!(phase = %me.phase, %error, "Failed receiving event");
                            }
                        }
                    }
                }
            }
        });
    }

    pub fn cancel(&mut self) {
        _ = self.cancel_channel.0.send(()).map_err(|error| {
            tracing::error!(phase = %self.phase, %error, "Failed sending cancel signal");
        });
    }

    fn act(&mut self, runtime: Runtime) -> crossbeam_channel::Receiver<InternalEvent> {
        tracing::debug!(phase = %self.phase, "Acting on phase");
        match &self.phase {
            Phase::TicketStats => self.ticket_stats(runtime),
            Phase::Done => crossbeam_channel::never(),
        }
    }

    fn event(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            InternalEvent::TicketStats(res) => match res {
                Ok(stats) => {
                    tracing::debug!(phase = %self.phase, %stats, "Got ticket stats");
                    _ = self.sender.send(Event::TicketStats(stats)).map_err(|error| {
                        tracing::error!(%error, "Failed sending ticket stats event");
                    });
                    self.phase = Phase::Done;
                    self.backoff = BackoffState::Inactive;
                    Ok(())
                }
                Err(error) => {
                    tracing::error!(phase = %self.phase, %error, "Failed getting ticket stats");
                    self.backoff = BackoffState::Active(ExponentialBackoff::default());
                    Ok(())
                }
            },
        }
    }

    fn ticket_stats(&mut self, runtime: Runtime) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        let network_specs = self.network_specs.clone();
        let priv_key = self.private_key.clone();
        let rpc_provider = self.rpc_provider.clone();
        thread::spawn(move || {
            let res = runtime.block_on(ticket_stats(priv_key, rpc_provider.to_string(), network_specs));
            _ = s.send(InternalEvent::TicketStats(res));
        });
        r
    }
}

impl Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Phase::TicketStats => write!(f, "TicketStats"),
            Phase::Done => write!(f, "Done"),
        }
    }
}

impl Display for InternalEvent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            InternalEvent::TicketStats(res) => match res {
                Ok(stats) => write!(f, "TicketStats({})", stats),
                Err(error) => write!(f, "TicketStatsError({})", error),
            },
        }
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::TicketStats(stats) => write!(f, "TicketStats({})", stats),
            Event::BackoffExhausted => write!(f, "BackoffExhausted"),
        }
    }
}

async fn ticket_stats(
    priv_key: ChainKeypair,
    rpc_provider: String,
    network_specs: NetworkSpecifications,
) -> Result<TicketStats, ChainError> {
    let client = GnosisRpcClient::with_url(priv_key, rpc_provider.as_str()).await?;
    network_specs
        .contracts
        .get_win_prob_ticket_price(&client.provider)
        .await
}
