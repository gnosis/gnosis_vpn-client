use backoff::{ExponentialBackoff, backoff::Backoff};
use edgli::hopr_lib::Address;
use edgli::hopr_lib::{Balance, WxHOPR};
use thiserror::Error;

use std::fmt::{self, Display};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use crate::balance::Balances;
use crate::hopr::{Hopr, HoprError};
use crate::info::Info;
use crate::log_output;

#[derive(Clone, Debug)]
pub enum Event {
    Info(Info),
    Balance(Balances),
    BackoffExhausted,
}

/// Represents the different phases of establishing a connection.
#[derive(Clone, Debug)]
enum Phase {
    Info,
    BalanceInitial,
    EnsureChannelFunding,
    Idle(SystemTime),
    BalanceRepeated,
}

#[derive(Debug)]
enum InternalEvent {
    Info(Info),
    Balance(Result<Balances, HoprError>),
    ChannelFundingDone,
    Tick,
}

#[derive(Clone, Debug)]
enum BackoffState {
    Inactive,
    Active(ExponentialBackoff),
    Triggered(ExponentialBackoff),
}

#[derive(Debug, Error)]
enum InternalError {
    #[error("Channel send error: {0}")]
    Send(#[from] crossbeam_channel::SendError<Event>),
    #[error("hopr-lib error: {0}")]
    Hopr(#[from] HoprError),
}

#[derive(Clone)]
pub struct Node {
    // message passing helper
    cancel_channel: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),

    // dynamic runtime data
    backoff: BackoffState,
    phase: Phase,

    // static input data
    edgli: Arc<Hopr>,
    sender: crossbeam_channel::Sender<Event>,
    channel_addresses: Vec<Address>,
    min_balance: Balance<WxHOPR>,
}

impl Node {
    pub fn new(
        sender: crossbeam_channel::Sender<Event>,
        edgli: Arc<Hopr>,
        channel_addresses: Vec<Address>,
        min_balance: Balance<WxHOPR>,
    ) -> Self {
        Node {
            cancel_channel: crossbeam_channel::bounded(1),
            backoff: BackoffState::Inactive,
            phase: Phase::Info,
            edgli,
            sender,
            channel_addresses,
            min_balance,
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
            Phase::Info => self.fetch_info(),
            Phase::BalanceInitial | Phase::BalanceRepeated => self.fetch_balance(),
            Phase::EnsureChannelFunding => self.ensure_channel_funding(),
            Phase::Idle(_system_time) => self.idle(),
        }
    }

    fn event(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            InternalEvent::Info(res) => {
                self.sender.send(Event::Info(res))?;
                self.phase = Phase::BalanceInitial;
                self.backoff = BackoffState::Inactive;
                Ok(())
            }
            InternalEvent::Balance(res) => {
                self.sender.send(Event::Balance(res?))?;
                match self.phase {
                    Phase::BalanceInitial => self.phase = Phase::EnsureChannelFunding,
                    Phase::BalanceRepeated => self.phase = Phase::Idle(SystemTime::now()),
                    _ => {}
                }
                self.backoff = BackoffState::Inactive;
                Ok(())
            }
            InternalEvent::ChannelFundingDone => {
                self.phase = Phase::Idle(SystemTime::now());
                Ok(())
            }
            InternalEvent::Tick => {
                self.phase = Phase::BalanceRepeated;
                Ok(())
            }
        }
    }

    fn fetch_info(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        let edgli = self.edgli.clone();
        thread::spawn(move || {
            let res = edgli.info();
            _ = s.send(InternalEvent::Info(res));
        });
        r
    }

    fn ensure_channel_funding(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        let edgli = self.edgli.clone();
        let channel_addresses = self.channel_addresses.clone();
        let min_balance = self.min_balance;
        thread::spawn(move || {
            for address in channel_addresses {
                _ = edgli.ensure_channel_open_and_funded(address, min_balance).map_err(|e| {
                    tracing::error!(%e, %address, "Failed to ensure channel open and funded");
                });
            }
            _ = s.send(InternalEvent::ChannelFundingDone);
        });
        r
    }

    fn fetch_balance(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        let edgli = self.edgli.clone();
        thread::spawn(move || {
            let res = edgli.balances();
            _ = s.send(InternalEvent::Balance(res));
        });
        r
    }

    fn idle(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(60));
            _ = s.send(InternalEvent::Tick);
        });
        r
    }
}

impl Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Phase::Info => write!(f, "Info"),
            Phase::BalanceInitial => write!(f, "BalanceInitial"),
            Phase::EnsureChannelFunding => write!(f, "EnsureChannelFunding"),
            Phase::BalanceRepeated => write!(f, "BalanceRepeated"),
            Phase::Idle(since) => write!(f, "Idle for {}", log_output::elapsed(since)),
        }
    }
}

impl Display for InternalEvent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            InternalEvent::Info(res) => write!(f, "Info({res:?})"),
            InternalEvent::Balance(res) => write!(f, "Balance({res:?})"),
            InternalEvent::ChannelFundingDone => write!(f, "ChannelFundingDone"),
            InternalEvent::Tick => write!(f, "Tick"),
        }
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::Info(info) => write!(f, "Info: {info}"),
            Event::Balance(balance) => write!(f, "Balance: {balance}"),
            Event::BackoffExhausted => write!(f, "BackoffExhausted"),
        }
    }
}
