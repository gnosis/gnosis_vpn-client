use backoff::{ExponentialBackoff, backoff::Backoff};
use crossbeam_channel;
use reqwest::blocking;
use thiserror::Error;
use uuid::Uuid;

use std::fmt::{self, Display};
use std::thread;

use crate::hopr::chain::{self, Chain};

#[derive(Clone, Debug)]
pub enum Event {
    BackoffExhausted,
}

/// Represents the different phases of establishing a connection.
#[derive(Clone, Debug)]
enum Phase {
    CheckNodeBalance,
    DeploySafe,
    FetchSafeModule,
}

#[derive(Debug)]
enum InternalEvent {
    NodeAddressBalance(Result<(), chain::Error>),
    DeploySafe(Result<Uuid, chain::Error>),
    FetchSafeModule(Result<String, chain::Error>),
}

#[derive(Clone, Debug)]
enum BackoffState {
    Inactive,
    Active(ExponentialBackoff),
    Triggered(ExponentialBackoff),
}

#[derive(Debug, Error)]
enum InternalError {}

#[derive(Clone)]
pub struct Onboarding {
    // message passing helper
    cancel_channel: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),

    // dynamic runtime data
    backoff: BackoffState,
    phase: Phase,

    // reuse http client
    client: blocking::Client,

    // static input data
    sender: crossbeam_channel::Sender<Event>,
    chain: Chain,
}

impl Onboarding {
    pub fn new(sender: crossbeam_channel::Sender<Event>, chain: Chain) -> Self {
        Onboarding {
            cancel_channel: crossbeam_channel::bounded(1),
            backoff: BackoffState::Inactive,
            phase: Phase::CheckNodeBalance,
            client: blocking::Client::new(),
            sender,
            chain,
        }
    }

    /// Query info once and continuously monitor balance
    pub fn run(&self) {
        let mut me = self.clone();
        thread::spawn(move || {
            loop {
                // Backoff handling
                // Inactive - no backoff was set, act up
                // Active - backoff was set and can trigger, don't act until backoff delay
                // Triggered - backoff was triggered, time to act up again keeping backoff active
                let (recv_event, recv_backoff) = match me.backoff.clone() {
                    BackoffState::Inactive => (me.act(), crossbeam_channel::never()),
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
                        (me.act(), crossbeam_channel::never())
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

    fn act(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        tracing::debug!(phase = %self.phase, "Acting on phase");
        match self.phase {
            Phase::CheckNodeBalance => self.fetch_node_address_balance(),
            Phase::DeploySafe => self.deploy_safe(),
            Phase::FetchSafeModule => self.fetch_safe_module(),
        }
    }

    fn event(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            InternalEvent::NodeAddressBalance(res) => {
                //    match res {
                //        Ok(balance) => {
                //            tracing::info!(phase = %self.phase, balance, "Node address balance fetched successfully");
                //            // Proceed to next phase
                //            self.phase = Phase::DeploySafe;
                //            // Reset backoff on success
                //            self.backoff = BackoffState::Inactive;
                //        }
                //        Err(error) => {
                //            tracing::error!(phase = %self.phase, %error, "Failed to fetch node address balance");
                //            // Backoff will be handled in the main loop
                //        }
                // }
                Ok(())
            }
            InternalEvent::DeploySafe(res) => {
                //match res {
                //        Ok(safe_id) => {
                //            tracing::info!(phase = %self.phase, safe_id = %safe_id, "Safe deployed successfully");
                //            // Proceed to next phase
                //            self.phase = Phase::FetchSafeModule;
                //            // Reset backoff on success
                //            self.backoff = BackoffState::Inactive;
                //        }
                //        Err(error) => {
                //            tracing::error!(phase = %self.phase, %error, "Failed to deploy safe");
                //            // Backoff will be handled in the main loop
                //        }
                //}
                Ok(())
            }
            InternalEvent::FetchSafeModule(res) => {
                //match res {
                //       Ok(module_address) => {
                //           tracing::info!(phase = %self.phase, module_address = %module_address, "Safe module fetched successfully");
                //           // Onboarding complete, could transition to an Idle phase or similar
                //           // For now, we just log completion
                //           tracing::info!(phase = %self.phase, "Onboarding process completed successfully");
                //           // Reset backoff on success
                //           self.backoff = BackoffState::Inactive;
                //       }
                //       Err(error) => {
                //           tracing::error!(phase = %self.phase, %error, "Failed to fetch safe module");
                //           // Backoff will be handled in the main loop
                //       }
                //}
                Ok(())
            }
        }
    }

    fn fetch_node_address_balance(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        let chain = self.chain.clone();
        thread::spawn(move || {
            let res = chain.node_address_balance(&client);
            _ = s.send(InternalEvent::NodeAddressBalance(res));
        });
        r
    }

    fn deploy_safe(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        let chain = self.chain.clone();
        thread::spawn(move || {
            // let res = chain.deploy_safe(&client);
            // _ = s.send(InternalEvent::DeploySafe(res));
        });
        r
    }

    fn fetch_safe_module(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        let chain = self.chain.clone();
        thread::spawn(move || {
            // let res = chain.fetch_safe_module(&client);
            // _ = s.send(InternalEvent::FetchSafeModule(res));
        });
        r
    }
}

impl Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Phase::CheckNodeBalance => write!(f, "CheckNodeBalance"),
            Phase::DeploySafe => write!(f, "DeploySafe"),
            Phase::FetchSafeModule => write!(f, "FetchSafeModule"),
        }
    }
}

impl Display for InternalEvent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            InternalEvent::NodeAddressBalance(res) => write!(f, "NodeAddressBalance({res:?})"),
            InternalEvent::DeploySafe(res) => write!(f, "DeploySafe({res:?})"),
            InternalEvent::FetchSafeModule(res) => write!(f, "FetchSafeModule({res:?})"),
        }
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::BackoffExhausted => write!(f, "BackoffExhausted"),
        }
    }
}
