use backoff::{ExponentialBackoff, ExponentialBackoffBuilder, backoff::Backoff};
use edgli::hopr_lib::Address;
use edgli::hopr_lib::{Balance, WxHOPR};
use thiserror::Error;

use std::fmt::{self, Display};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::hopr::Hopr;
use crate::hopr::api::ChannelError;

#[derive(Clone, Debug)]
pub enum Event {
    ChannelFundedOk(Address),
    ChannelNotFunded(Address),
    BackoffExhausted,
    Done,
}

/// Represents the different phases of establishing a connection.
#[derive(Clone, Debug)]
enum Phase {
    ChannelFunding,
    FailedChannelFunding(Vec<Address>),
}

#[derive(Debug)]
enum InternalEvent {
    ChannelFunding(Vec<ChannelResult>),
}

#[derive(Debug)]
struct ChannelResult {
    address: Address,
    res: Result<(), ChannelError>,
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
pub struct ChannelFunding {
    // message passing helper
    cancel_channel: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),

    // dynamic runtime data
    backoff: BackoffState,
    phase: Phase,

    // static input data
    edgli: Arc<Hopr>,
    sender: crossbeam_channel::Sender<Event>,
    channel_addresses: Vec<Address>,
    ticket_price: Balance<WxHOPR>,
    min_stake_threshold: Balance<WxHOPR>,
}

impl ChannelFunding {
    pub fn new(
        sender: crossbeam_channel::Sender<Event>,
        edgli: Arc<Hopr>,
        channel_addresses: Vec<Address>,
        ticket_price: Balance<WxHOPR>,
        min_stake_threshold: Balance<WxHOPR>,
    ) -> Self {
        ChannelFunding {
            cancel_channel: crossbeam_channel::bounded(1),
            backoff: BackoffState::Inactive,
            phase: Phase::ChannelFunding,
            edgli,
            sender,
            channel_addresses,
            ticket_price,
            min_stake_threshold,
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
        match self.phase.clone() {
            Phase::ChannelFunding => self.channel_funding(self.channel_addresses.clone()),
            Phase::FailedChannelFunding(failed_channels) => self.channel_funding(failed_channels),
        }
    }

    fn event(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            InternalEvent::ChannelFunding(results) => {
                let mut failed_channels = vec![];
                for ChannelResult { address, res } in results {
                    match res {
                        Err(ChannelError::Fund(e)) => {
                            self.backoff = BackoffState::Active(short_backoff());
                            failed_channels.push(address);
                        }
                        Err(ChannelError::PendingToClose) => {
                            self.backoff = BackoffState::Active(pending_to_close_backoff());
                            failed_channels.push(address);
                        }
                        Err(ChannelError::Open(e)) => {
                            self.backoff = BackoffState::Active(short_backoff());
                            failed_channels.push(address);
                        }
                        Err(err) => {
                            tracing::error!(phase = %self.phase, address = %address, %err, "ensure channel funding error");
                            self.backoff = BackoffState::Active(short_backoff());
                            _ = self.sender.send(Event::ChannelNotFunded(address));
                        }
                        Ok(()) => {
                            _ = self.sender.send(Event::ChannelFundedOk(address));
                        }
                    }
                }

                if failed_channels.is_empty() {
                    self.backoff = BackoffState::Inactive;
                    _ = self.sender.send(Event::Done).map_err(|error| {
                        tracing::error!(%error, "Failed sending done event");
                    });
                } else {
                    self.phase = Phase::FailedChannelFunding(failed_channels);
                }
                Ok(())
            }
        }
    }

    fn channel_funding(&mut self, channel_addresses: Vec<Address>) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        let edgli = self.edgli.clone();
        let ticket_price = self.ticket_price;
        let min_stake_threshold = self.min_stake_threshold;
        thread::spawn(move || {
            let mut results = Vec::with_capacity(channel_addresses.len());
            for address in channel_addresses {
                let res = edgli.ensure_channel_open_and_funded(address, ticket_price, min_stake_threshold);
                results.push(ChannelResult { address, res });
            }
            _ = s.send(InternalEvent::ChannelFunding(results)).map_err(|error| {
                tracing::error!(%error, "Failed sending channel funding results");
            })
        });
        r
    }
}

impl Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Phase::ChannelFunding => write!(f, "ChannelFunding"),
            Phase::FailedChannelFunding(failed_channels) => {
                write!(f, "FailedChannelFunding(")?;
                for (i, address) in failed_channels.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", address)?;
                }
                write!(f, ")")
            }
        }
    }
}

impl Display for InternalEvent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            InternalEvent::ChannelFunding(results) => {
                write!(f, "ChannelFunding(")?;
                for (i, ChannelResult { address, res }) in results.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    match res {
                        Ok(()) => write!(f, "{}: Ok", address)?,
                        Err(error) => write!(f, "{}: Err({})", address, error)?,
                    }
                }
                write!(f, ")")
            }
        }
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::ChannelFundedOk(address) => write!(f, "ChannelFundedOk({})", address),
            Event::ChannelNotFunded(address) => write!(f, "ChannelNotFunded({})", address),
            Event::BackoffExhausted => write!(f, "BackoffExhausted"),
            Event::Done => write!(f, "Done"),
        }
    }
}

fn short_backoff() -> ExponentialBackoff {
    ExponentialBackoffBuilder::new()
        .with_initial_interval(Duration::from_secs(2))
        .with_randomization_factor(0.2)
        .with_multiplier(1.1)
        .with_max_elapsed_time(Some(Duration::from_secs(1 * 60))) // 1 minute
        .build()
}

fn pending_to_close_backoff() -> ExponentialBackoff {
    ExponentialBackoffBuilder::new()
        .with_initial_interval(Duration::from_secs(2))
        .with_randomization_factor(0.2)
        .with_multiplier(1.1)
        .with_max_elapsed_time(Some(Duration::from_secs(10 * 60))) // 10 minutes
        .build()
}
