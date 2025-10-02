use alloy::primitives::U256;
use backoff::{ExponentialBackoff, backoff::Backoff};
use crossbeam_channel;
use edgli::hopr_lib::Address;
use edgli::hopr_lib::exports::crypto::types::prelude::ChainKeypair;
use rand::Rng;
use thiserror::Error;
use tokio::runtime::Runtime;
use url::Url;

use std::fmt::{self, Display};
use std::thread;
use std::time::Duration;

use crate::balance;
use crate::chain::client::GnosisRpcClient;
use crate::chain::contracts::{
    CheckBalanceInputs, CheckBalanceResult, SafeModuleDeploymentInputs, SafeModuleDeploymentResult,
};
use crate::chain::errors::ChainError;
use crate::hopr::config;

#[derive(Clone, Debug)]
pub enum Event {
    Balance(balance::PreSafe),
    SafeModule(config::SafeModule),
    BackoffExhausted,
}

/// Represents the different phases of establishing a connection.
#[derive(Clone, Debug)]
enum Phase {
    CheckAccountBalance,
    WaitAccountBalance,
    DeploySafe(balance::PreSafe),
    Done,
}

#[derive(Debug)]
enum InternalEvent {
    NodeAddressBalance(Result<CheckBalanceResult, ChainError>),
    TickAccountBalance,
    SafeDeployment(Result<SafeModuleDeploymentResult, ChainError>),
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
}

#[derive(Clone)]
pub struct Onboarding {
    // message passing helper
    cancel_channel: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),

    // dynamic runtime data
    backoff: BackoffState,
    phase: Phase,

    // static input data
    sender: crossbeam_channel::Sender<Event>,
    private_key: ChainKeypair,
    rpc_provider: Url,
    node_address: Address,

    // dynamic runtime data
    nonce: U256,
}

impl Onboarding {
    pub fn new(
        sender: crossbeam_channel::Sender<Event>,
        private_key: ChainKeypair,
        rpc_provider: Url,
        node_address: Address,
    ) -> Self {
        Onboarding {
            cancel_channel: crossbeam_channel::bounded(1),
            backoff: BackoffState::Inactive,
            phase: Phase::CheckAccountBalance,
            sender,
            private_key,
            rpc_provider,
            node_address,
            nonce: U256::from(rand::rng().random_range(1..1_000)),
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
            Phase::CheckAccountBalance => self.fetch_node_address_balance(runtime),
            Phase::WaitAccountBalance => self.wait_account_balance(),
            Phase::DeploySafe(balance) => self.deploy_safe(runtime, balance.clone()),
            Phase::Done => crossbeam_channel::never(),
        }
    }

    fn event(&mut self, event: InternalEvent) -> Result<(), InternalError> {
        match event {
            InternalEvent::NodeAddressBalance(res) => {
                let balance: balance::PreSafe = res?.into();
                self.backoff = BackoffState::Inactive;
                if balance.node_xdai.is_zero() || balance.node_wxhopr.is_zero() {
                    self.phase = Phase::WaitAccountBalance;
                } else {
                    self.phase = Phase::DeploySafe(balance.clone());
                }
                _ = self.sender.send(Event::Balance(balance)).map_err(|error| {
                    tracing::error!(%error, "Failed sending balance event");
                });
                Ok(())
            }
            InternalEvent::TickAccountBalance => {
                self.phase = Phase::CheckAccountBalance;
                Ok(())
            }
            InternalEvent::SafeDeployment(res) => {
                let safe_module: config::SafeModule = res?.into();
                self.backoff = BackoffState::Inactive;
                self.phase = Phase::Done;
                _ = self.sender.send(Event::SafeModule(safe_module)).map_err(|error| {
                    tracing::error!(%error, "Failed sending safe module event");
                });
                Ok(())
            }
        }
    }

    fn fetch_node_address_balance(&mut self, runtime: Runtime) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        let priv_key = self.private_key.clone();
        let node_address = self.node_address;
        let rpc_provider = self.rpc_provider.clone();
        thread::spawn(move || {
            let res = runtime.block_on(check_balance(priv_key, rpc_provider.to_string(), node_address));
            _ = s.send(InternalEvent::NodeAddressBalance(res));
        });
        r
    }

    fn deploy_safe(
        &mut self,
        runtime: Runtime,
        balance: balance::PreSafe,
    ) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        if let BackoffState::Inactive = self.backoff {
            self.backoff = BackoffState::Active(ExponentialBackoff::default());
        }
        let priv_key = self.private_key.clone();
        let node_address = self.node_address;
        let rpc_provider = self.rpc_provider.clone();
        self.nonce += U256::from(1);
        let nonce = self.nonce;
        let token_u256 = balance.node_wxhopr.amount();
        let token_bytes: [u8; 32] = token_u256.to_big_endian();
        let token_amount: U256 = U256::from_be_bytes::<32>(token_bytes);
        thread::spawn(move || {
            let res = runtime.block_on(safe_module_deployment(
                priv_key,
                rpc_provider.to_string(),
                node_address,
                nonce,
                token_amount,
            ));
            _ = s.send(InternalEvent::SafeDeployment(res));
        });
        r
    }

    fn wait_account_balance(&mut self) -> crossbeam_channel::Receiver<InternalEvent> {
        let (s, r) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(10));
            _ = s.send(InternalEvent::TickAccountBalance);
        });
        r
    }
}

impl Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Phase::CheckAccountBalance => write!(f, "CheckAccountBalance"),
            Phase::WaitAccountBalance => write!(f, "WaitAccountBalance"),
            Phase::DeploySafe(balance) => write!(f, "DeploySafe({balance})"),
            Phase::Done => write!(f, "Done"),
        }
    }
}

impl Display for InternalEvent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            InternalEvent::NodeAddressBalance(res) => write!(f, "NodeAddressBalance({res:?})"),
            InternalEvent::TickAccountBalance => write!(f, "TickAccountBalance"),
            InternalEvent::SafeDeployment(res) => write!(f, "SafeDeployment({res:?})"),
        }
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::BackoffExhausted => write!(f, "BackoffExhausted"),
            Event::Balance(balance) => write!(f, "Balance({balance})"),
            Event::SafeModule(safe_module) => write!(f, "SafeModule({safe_module:?})"),
        }
    }
}

async fn check_balance(
    priv_key: ChainKeypair,
    rpc_provider: String,
    node_address: Address,
) -> Result<CheckBalanceResult, ChainError> {
    let client = GnosisRpcClient::with_url(priv_key, rpc_provider.as_str()).await?;
    let check_balance_inputs = CheckBalanceInputs::new(node_address.into(), node_address.into());
    check_balance_inputs.check(&client.provider).await
}

async fn safe_module_deployment(
    priv_key: ChainKeypair,
    rpc_provider: String,
    node_address: Address,
    nonce: U256,
    token_amount: U256,
) -> Result<SafeModuleDeploymentResult, ChainError> {
    let client = GnosisRpcClient::with_url(priv_key, rpc_provider.as_str()).await?;
    let safe_module_deployment_inputs = SafeModuleDeploymentInputs::new(nonce, token_amount, vec![node_address.into()]);
    safe_module_deployment_inputs.deploy(&client.provider).await
}
