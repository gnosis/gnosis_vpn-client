use backoff::{ExponentialBackoff, ExponentialBackoffBuilder, backoff::Backoff};
use edgli::hopr_lib::Address;
use edgli::hopr_lib::{Balance, WxHOPR};
use thiserror::Error;

use std::fmt::{self, Display};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::hopr::{Hopr, HoprError};
use crate::ticket_stats::{self, TicketStats};

#[derive(Clone, Debug)]
pub enum Event {
    TicketStats(TicketStats),
    ChannelFundedOk(Address),
    ChannelNotFunded(Address),
    BackoffExhausted,
    Done,
}

/// Represents the different phases of establishing a connection.
#[derive(Clone, Debug)]
enum Phase {
    TicketStats,
    ChannelFunding(Balance<WxHOPR>),
    FailedChannelFunding {
        ticket_price: Balance<WxHOPR>,
        failed_channels: Vec<Address>,
    },
}

#[derive(Debug)]
enum InternalEvent {
    TicketStats(Result<TicketStats, HoprError>),
    ChannelFunding(Vec<ChannelResult>),
}

#[derive(Debug)]
struct ChannelResult {
    address: Address,
    res: Result<(), HoprError>,
}

#[derive(Clone, Debug)]
enum BackoffState {
    Inactive,
    Active(ExponentialBackoff),
    Triggered(ExponentialBackoff),
}

#[derive(Debug, Error)]
enum InternalError {
    #[error("Invalid phase for action")]
    UnexpectedPhase,
    #[error("Ticket stats error: {0}")]
    TicketStats(#[from] ticket_stats::Error),
}

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
}

impl ChannelFunding {
    pub fn new(sender: crossbeam_channel::Sender<Event>, edgli: Arc<Hopr>, channel_addresses: Vec<Address>) -> Self {
        ChannelFunding {
            cancel_channel: crossbeam_channel::bounded(1),
            backoff: BackoffState::Inactive,
            phase: Phase::TicketStats,
            edgli,
            sender,
            channel_addresses,
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
            Phase::TicketStats => self.ticket_stats(),
            Phase::ChannelFunding(ticket_price) => self.channel_funding(self.channel_addresses.clone(), ticket_price),
            Phase::FailedChannelFunding {
                ticket_price,
                failed_channels,
            } => self.channel_funding(failed_channels, ticket_price),
        }
    }

    fn event(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            InternalEvent::ChannelFunding(results) => match self.phase.clone() {
                Phase::ChannelFunding(ticket_price) | Phase::FailedChannelFunding { ticket_price, .. } => {
                    let mut failed_channels = vec![];
                    for ChannelResult { address, res } in results {
                        match res {
                            Ok(()) => {
                                _ = self.sender.send(Event::ChannelFundedOk(address));
                            }

                            Err(error) => {
                                tracing::error!(phase = %self.phase, address = %address, %error, "Channel funding failed");
                                failed_channels.push(address);
                                _ = self.sender.send(Event::ChannelNotFunded(address));
                            }
                        }
                    }
                    if failed_channels.is_empty() {
                        self.backoff = BackoffState::Inactive;
                        _ = self.sender.send(Event::Done).map_err(|error| {
                            tracing::error!(%error, "Failed sending done event");
                        });
                    } else {
                        self.backoff = BackoffState::Active(channel_backoff());
                        self.phase = Phase::FailedChannelFunding {
                            failed_channels,
                            ticket_price,
                        };
                    }
                    Ok(())
                }
                _ => Err(InternalError::UnexpectedPhase),
            },
            InternalEvent::TicketStats(res) => match self.phase.clone() {
                Phase::TicketStats => match res {
                    Ok(stats) => {
                        tracing::debug!(phase = %self.phase, %stats, "Got ticket stats");
                        _ = self.sender.send(Event::TicketStats(stats)).map_err(|error| {
                            tracing::error!(%error, "Failed sending ticket stats event");
                        });
                        let ticket_price = stats.ticket_price()?;
                        self.phase = Phase::ChannelFunding(ticket_price);
                        self.backoff = BackoffState::Inactive;
                        Ok(())
                    }
                    Err(error) => {
                        tracing::error!(phase = %self.phase, %error, "Failed getting ticket stats");
                        self.backoff = BackoffState::Active(ExponentialBackoff::default());
                        Ok(())
                    }
                },
                _ => Err(InternalError::UnexpectedPhase),
            },
        }
    }

    fn ticket_stats(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        let edgli = self.edgli.clone();
        thread::spawn(move || {
            let res = edgli.get_ticket_stats();
            _ = s.send(InternalEvent::TicketStats(res)).map_err(|error| {
                tracing::error!(%error, "Failed sending ticket stats");
            })
        });
        r
    }

    fn channel_funding(
        &mut self,
        channel_addresses: Vec<Address>,
        ticket_price: Balance<WxHOPR>,
    ) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        let edgli = self.edgli.clone();
        thread::spawn(move || {
            let mut results = Vec::with_capacity(channel_addresses.len());
            for address in channel_addresses {
                let res = edgli.ensure_channel_open_and_funded(address, ticket_price);
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
            Phase::TicketStats => write!(f, "TicketStats"),
            Phase::ChannelFunding(ticket_price) => write!(f, "ChannelFunding({})", ticket_price),
            Phase::FailedChannelFunding {
                ticket_price,
                failed_channels,
            } => write!(
                f,
                "FailedChannelFunding({}, failed: {})",
                ticket_price,
                failed_channels
                    .iter()
                    .map(|addr| addr.to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            ),
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
            InternalEvent::TicketStats(res) => match res {
                Ok(stats) => write!(f, "TicketStats({})", stats),
                Err(error) => write!(f, "TicketStats(Err({}))", error),
            },
        }
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::TicketStats(stats) => write!(f, "TicketStats({})", stats),
            Event::ChannelFundedOk(address) => write!(f, "ChannelFundedOk({})", address),
            Event::ChannelNotFunded(address) => write!(f, "ChannelNotFunded({})", address),
            Event::BackoffExhausted => write!(f, "BackoffExhausted"),
            Event::Done => write!(f, "Done"),
        }
    }
}

fn channel_backoff() -> ExponentialBackoff {
    ExponentialBackoffBuilder::new()
        .with_initial_interval(Duration::from_secs(3))
        .with_randomization_factor(0.3)
        .with_multiplier(1.5)
        .with_max_elapsed_time(Some(Duration::from_secs(10 * 60))) // 10 minutes
        .build()
}
