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
    Syncing(float),
}

/// Represents the different phases of establishing a connection.
#[derive(Clone, Debug)]
enum Phase {
    Metrics,
    Idle(SystemTime),
}

#[derive(Debug)]
enum InternalEvent {
    Tick,
}

#[derive(Debug, Error)]
enum InternalError {}

#[derive(Clone)]
pub struct Metrics {
    // message passing helper
    cancel_channel: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),

    // dynamic runtime data
    phase: Phase,

    // static input data
    edgli: Arc<Hopr>,
    sender: crossbeam_channel::Sender<Event>,
}

impl Metrics {
    pub fn new(sender: crossbeam_channel::Sender<Event>, edgli: Arc<Hopr>) -> Self {
        Metrics {
            cancel_channel: crossbeam_channel::bounded(1),
            phase: Phase::Info,
            edgli,
            sender,
        }
    }

    /// Query info once and continuously monitor balance
    pub fn run(&self) {
        let mut me = self.clone();
        thread::spawn(move || {
            loop {
                let recv_event = me.act();
                crossbeam_channel::select! {
                    // checking on cancel signal
                    recv(me.cancel_channel.1) -> _ => break,
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
            Phase::Metrics => self.metrics(),
            Phase::Idle(_system_time) => self.idle(),
        }
    }

    fn event(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            InternalEvent::Metrics(res) => {
                self.phase = Phase::Idle(SystemTime::now());
                unimplemented!(" todo ");
                // self.sender.send(Event::Info(res))?;
                Ok(())
            }
            InternalEvent::Tick => {
                self.phase = Phase::Metrics;
                Ok(())
            }
        }
    }

    fn metrics(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let edgli = self.edgli.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let res = edgli.metrics().map_err(HoprError::from);
            _ = s.send(InternalEvent::Metrics(res));
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
            Phase::Balance => write!(f, "Balance"),
            Phase::Idle(since) => write!(f, "Idle for {}", log_output::elapsed(since)),
        }
    }
}

impl Display for InternalEvent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            InternalEvent::Info(res) => write!(f, "Info({res:?})"),
            InternalEvent::Balance(res) => write!(f, "Balance({res:?})"),
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
