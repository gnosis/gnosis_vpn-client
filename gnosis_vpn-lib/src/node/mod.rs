#[derive(Clone, Copy, Debug)]
pub enum Event {
    Info(Info)
    Balance(Balance),
}

/// Represents the different phases of establishing a connection.
#[derive(Clone, Debug)]
enum Phase {
    Info,
    Balance,
    Idle,
}

#[derive(Debug)]
enum InternalEvent {
    Info(Result<Info, info::Error>),
    Balance(Result<Balance, balance::Error>),
    Tick,
}

#[derive(Clone, Debug)]
enum BackoffState {
    Inactive,
    Active(ExponentialBackoff),
    Triggered(ExponentialBackoff),
}

#[derive(Clone, Debug)]
pub struct Node {
    // message passing helper
    cancel_channel: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),

    // reuse http client
    client: blocking::Client,

    // dynamic runtime data
    backoff: BackoffState,

    // static input data
    entry_node: EntryNode,
    sender: crossbeam_channel::Sender<Event>,
}

#[derive(Debug, Error)]
enum InternalError {
    #[error("Invalid phase for event")]
    UnexpectedPhase,
    #[error("Channel send error: {0}")]
    SendError(#[from] crossbeam_channel::SendError<Event>),
}


impl Node {
    pub fn new(entry_node: EntryNode, sender: crossbeam_channel::Sender<Event>) -> Self {
        Node {
            cancel_channel: crossbeam_channel::bounded(1),
            client: blocking::Client::new(),
            backoff: BackoffState::Inactive,
            entry_node,
            sender,
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
                            tracing::debug!(phase = me.phase, ?backoff, delay = ?delay, "Triggering backoff delay");
                            me.backoff = BackoffState::Triggered(backoff);
                            (crossbeam_channel::never(), crossbeam_channel::after(delay))
                        }
                        None => {
                            me.backoff = BackoffState::Inactive;
                            tracing::error!(phase = me.phase, "Unrecoverable error: backoff exhausted");
                            _ = me.sender.send(Event::Broken).map_err(|error| {
                                tracing::error!(%error, "Failed sending broken event");
                            });
                            (crossbeam_channel::never(), crossbeam_channel::never())
                        }
                    },
                    BackoffState::Triggered(backoff) => {
                        tracing::debug!(phase = me.phase, ?backoff, "Activating backoff");
                        me.backoff = BackoffState::Active(backoff);
                        (me.act(), crossbeam_channel::never())
                    }
                };

                crossbeam_channel::select! {
                    // checking on cancel signal
                    recv(me.cancel_channel.1) -> _ => break,
                    recv(recv_backoff) -> _ => {
                        tracing::debug!(phase = me.phase, "Backoff delay hit - loop to act");
                    },
                    recv(recv_event) -> res => {
                        match res {
                            Ok(event) => {
                                tracing::debug!(phase = me.phase, %event, "Received event");
                                _ = me.act(event).map_err(|error| {
                                    tracing::error!(phase = me.phase, %error, "Failed to process event");
                                });
                            }
                            Err(error) => {
                                tracing::error!(phase = me.phase, %error, "Failed receiving event");
                            }
                        }
                    }
                }
            }
        });
    }

    pub fn cancel(&mut self) {
        self.cancel_channel.0.send(()).map_err(|error| {
            tracing::error!(phase = self.phase, %error, "Failed sending cancel signal");
        });
    }

    fn act(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        tracing::debug!(phase = self.phase, "Acting on phase");
        match self.phase.clone() {
            Phase::Info => self.fetch_info(),
            Phase::Balance => self.fetch_balance(),
            Phase::Idle => self.idle(),
        }
    }

    fn event(&self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            InternalEvent::Info(res) => {
                self.sender.send(Event::Info(res?))?;
                self.phase = Phase::Balance;
                self.backoff = BackoffState::Inactive;
                Ok(())
            },
            InternalEvent::Balance(res) => {
                self.sender.send(Event::Balance(res?))?;
                self.phase = Phase::Idle;
                self.backoff = BackoffState::Inactive;
                Ok(())
            },
            InternalEvent::Tick => {
                self.phase = Phase::Balance;
                Ok(())
            }
        }
    }

    fn fetch_info(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        thread::spawn(move || {
            let res = Info::gather(&client, &self.entry_node);
            _ = s.send(InternalEvent::Info(res));
        });
        r
    }

    fn fetch_balance(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let client = self.client.clone();
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        thread::spawn(move || {
            let res = Balance::calc_for_node(&client, &self.entry_node);
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
