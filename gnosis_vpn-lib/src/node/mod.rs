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
}

#[derive(Clone, Debug)]
enum BackoffState {
    Inactive,
    Active(ExponentialBackoff),
    Triggered(ExponentialBackoff),
    NotRecoverable(String),
}

#[derive(Clone, Debug)]
pub struct Node {
    // reuse http client
    client: blocking::Client,

    // dynamic runtime data
    backoff: BackoffState,

    // static input data
    entry_node: EntryNode,
    sender: crossbeam_channel::Sender<Event>,
}

impl Node {
    pub fn new(entry_node: EntryNode, sender: crossbeam_channel::Sender<Event>) -> Self {
        Node {
            client: blocking::Client::new(),
            backoff: BackoffState::Inactive,
            entry_node,
            sender,
        }
    }

    pub fn run(&self) {
        let mut me = self.clone();
        thread::spawn(move || {
            loop {
                // Backoff handling
                // Inactive - no backoff was set, act up
                // Active - backoff was set and can trigger, don't act until backoff delay
                // Triggered - backoff was triggered, time to act up again keeping backoff active
                // NotRecoverable - critical error, halt connection establishment
                let (recv_event, recv_backoff) = match me.backoff.clone() {
                    BackoffState::Inactive => (me.act_up(), crossbeam_channel::never()),
                    BackoffState::Active(mut backoff) => match backoff.next_backoff() {
                        Some(delay) => {
                            tracing::debug!(phase = "up", ?backoff, delay = ?delay, "Triggering backoff delay");
                            me.backoff = BackoffState::Triggered(backoff);
                            (crossbeam_channel::never(), crossbeam_channel::after(delay))
                        }
                        None => {
                            me.backoff = BackoffState::Inactive;
                            tracing::error!(phase = "up", "Unrecoverable error: backoff exhausted");
                            _ = me.sender.send(Event::Broken).map_err(|error| {
                                tracing::error!(%error, "Failed sending broken event");
                            });
                            (crossbeam_channel::never(), crossbeam_channel::never())
                        }
                    },
                    BackoffState::Triggered(backoff) => {
                        tracing::debug!(phase = "up", ?backoff, "Activating backoff");
                        me.backoff = BackoffState::Active(backoff);
                        (me.act_up(), crossbeam_channel::never())
                    }
                    BackoffState::NotRecoverable(error) => {
                        tracing::error!(phase = "up", %error, "Unrecoverable error: connection broken");
                        _ = me.sender.send(Event::Broken).map_err(|error| {
                            tracing::error!(phase = "up", %error, "Failed sending dismantled event");
                        });
                        (crossbeam_channel::never(), crossbeam_channel::never())
                    }
                };
                // main listening loop
                crossbeam_channel::select! {
                    // waiting on dismantle signal for providing runtime data
                    recv(me.establish_channel.1) -> res => {
                        match res {
                            Ok(()) => {
                                match me.dismantle_channel.0.send(me.phase_up) {
                                    Ok(()) => (),
                                    Err(error) => {
                                        tracing::error!(phase = "up", %error, "Unrecoverable error: sending connection data on dismantle channel");
                                        _ = me.sender.send(Event::Dismantled).map_err(|error| {
                                            tracing::error!(%error, "Failed sending dismantled event");
                                        });
                                    }
                                }
                                break;
                            }
                            Err(error) => {
                                tracing::error!(phase = "up", %error, "Failed receiving signal on establish channel");
                            }
                        }
                    },
                    recv(recv_backoff) -> _ => {
                        tracing::debug!(phase = "up", "Backoff delay hit - loop to act");
                    },
                    recv(recv_event) -> res => {
                        match res {
                            Ok(event) => {
                                tracing::debug!(phase = "up", %event, "Received event");
                                _ = me.act_event_up(event).map_err(|error| {
                                    tracing::error!(phase = "up", %error, "Failed to process event");
                                });
                            }
                            Err(error) => {
                                tracing::error!(phase = "up", %error, "Failed receiving event");
                            }
                        }
                    }
                }
            }
        });
        let info = self.fetch_info()?;
        self.sender.send(Event::Info(info))?;

        let balance = self.fetch_balance()?;
        self.sender.send(Event::Balance(balance))?;

        Ok(())
    }
}
