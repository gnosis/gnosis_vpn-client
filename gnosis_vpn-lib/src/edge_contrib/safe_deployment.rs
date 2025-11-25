//! Safe Module Deployment
//!
//! This module provides functionality for deploying Safe modules on HOPR networks.
//! This code is intended to be contributed to the hoprnet/edge-client repository.

use alloy::{
    primitives::{Address, B256, Bytes, U256, address, hex},
    providers::Provider,
    sol,
    sol_types::SolType,
};
use thiserror::Error;

// wxHOPR Token contract address on Gnosis Chain
pub const WXHOPR_TOKEN_ADDRESS: Address = address!("0xD4fdec44DB9D44B8f2b6d529620f9C0C7066A2c1");

// Default target suffix to be appended to Channels contract address
pub const DEFAULT_TARGET_SUFFIX: [u8; 12] = hex!("010103020202020202020202");

pub const DEPLOY_SAFE_MODULE_AND_INCLUDE_NODES_IDENTIFIER: [u8; 32] =
    hex!("0105b97dcdf19d454ebe36f91ed516c2b90ee79f4a46af96a0138c1f5403c1cc");

// Interface for send() function of wxHOPR token contract
sol! {
    #[sol(rpc)]
    contract Token {
        function send(address recipient, uint256 amount, bytes memory data) external;
    }
}

sol!(
    #[sol(rpc)]
    contract HoprNodeStakeFactory {
        // Emit when a new module is created
        event NewHoprNodeStakeModule(address instance);
        // Emit when a new safe proxy is created
        event NewHoprNodeStakeSafe(address instance);
    }
);

type UserDataTuple = sol! { tuple(bytes32, uint256, bytes32, address[]) };

// ABI encoding offset size in bytes - the first 32 bytes contain the data offset
const ABI_OFFSET_SIZE: usize = 32;

#[derive(Debug, Error)]
pub enum SafeDeploymentError {
    #[error("Contract error: {0}")]
    Contract(String),
    #[error("Failed to decode event: {0}")]
    DecodeEvent(String),
    #[error("Transport error: {0}")]
    Transport(String),
}

/// Configuration for a specific HOPR network
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkConfig {
    pub channels_contract_address: Address,
    pub node_stake_factory_address: Address,
}

impl NetworkConfig {
    /// Build the default target as bytes32 by concatenating channel contract address with DEFAULT_TARGET_SUFFIX
    pub fn build_default_target(&self) -> B256 {
        let mut target_bytes = [0u8; 32];
        // Copy the 20 bytes of the address
        target_bytes[0..20].copy_from_slice(self.channels_contract_address.as_slice());
        // Copy the 12 bytes of the suffix
        target_bytes[20..32].copy_from_slice(&DEFAULT_TARGET_SUFFIX);
        B256::from(target_bytes)
    }
}

/// Configuration for deploying a Safe module
#[derive(Clone, Debug)]
pub struct SafeDeploymentConfig {
    /// Amount of tokens to stake
    pub token_amount: U256,
    /// Random nonce for deployment
    pub nonce: U256,
    /// Admin addresses for the Safe
    pub admins: Vec<Address>,
}

impl SafeDeploymentConfig {
    pub fn new(nonce: U256, token_amount: U256, admins: Vec<Address>) -> Self {
        Self {
            nonce,
            token_amount,
            admins,
        }
    }

    /// Build user data for Safe module deployment
    ///
    /// Equivalent to Solidity:
    /// `abi.encode(factory.DEPLOYSAFEMODULE_FUNCTION_IDENTIFIER(), nonce, DEFAULT_TARGET, admins)`
    pub fn build_user_data(&self, network_config: &NetworkConfig) -> Bytes {
        let default_target = network_config.build_default_target();

        let user_data_with_offset = UserDataTuple::abi_encode(&(
            DEPLOY_SAFE_MODULE_AND_INCLUDE_NODES_IDENTIFIER,
            self.nonce,
            default_target,
            self.admins.clone(),
        ));

        // Remove the ABI encoding offset (first 32 bytes)
        let user_data = user_data_with_offset[ABI_OFFSET_SIZE..].to_vec();
        Bytes::from(user_data)
    }
}

/// Result of a Safe module deployment
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafeDeploymentResult {
    pub tx_hash: B256,
    pub safe_address: Address,
    pub module_address: Address,
}

/// Safe module deployer
pub struct SafeDeployer;

impl SafeDeployer {
    /// Deploy a Safe module on the specified network
    ///
    /// # Arguments
    /// * `provider` - The blockchain provider
    /// * `config` - The deployment configuration
    /// * `network_config` - The network-specific configuration
    ///
    /// # Returns
    /// The deployment result containing transaction hash, safe address, and module address
    pub async fn deploy<P>(
        provider: &P,
        config: &SafeDeploymentConfig,
        network_config: &NetworkConfig,
    ) -> Result<SafeDeploymentResult, SafeDeploymentError>
    where
        P: Provider + Clone,
    {
        let token_instance = Token::new(WXHOPR_TOKEN_ADDRESS, provider.clone());
        let user_data = config.build_user_data(network_config);

        // Deploy the safe module by calling send on the wxHOPR token contract
        let pending_tx = token_instance
            .send(
                network_config.node_stake_factory_address,
                config.token_amount,
                user_data,
            )
            .send()
            .await
            .map_err(|e| SafeDeploymentError::Contract(e.to_string()))?;

        let receipt = pending_tx
            .get_receipt()
            .await
            .map_err(|e| SafeDeploymentError::Transport(e.to_string()))?;

        // Check if transaction was successful
        if !receipt.status() {
            return Err(SafeDeploymentError::Contract(
                "Transaction failed - check if sufficient tokens were provided".to_string(),
            ));
        }

        let maybe_safe_log = receipt.decoded_log::<HoprNodeStakeFactory::NewHoprNodeStakeSafe>();
        let Some(safe_log) = maybe_safe_log else {
            return Err(SafeDeploymentError::DecodeEvent(
                "NewHoprNodeStakeSafe event not found in transaction receipt".to_string(),
            ));
        };

        let maybe_module_log = receipt.decoded_log::<HoprNodeStakeFactory::NewHoprNodeStakeModule>();
        let Some(module_log) = maybe_module_log else {
            return Err(SafeDeploymentError::DecodeEvent(
                "NewHoprNodeStakeModule event not found in transaction receipt".to_string(),
            ));
        };

        Ok(SafeDeploymentResult {
            tx_hash: receipt.transaction_hash,
            safe_address: safe_log.instance,
            module_address: module_log.instance,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_config_build_default_target() {
        let config = NetworkConfig {
            channels_contract_address: address!("0x693Bac5ce61c720dDC68533991Ceb41199D8F8ae"),
            node_stake_factory_address: address!("0x048D04C9f5F74d65e76626B943779DEC6EdCEFeC"),
        };

        let target = config.build_default_target();
        
        // First 20 bytes should be the channels contract address
        assert_eq!(&target[0..20], config.channels_contract_address.as_slice());
        
        // Last 12 bytes should be the DEFAULT_TARGET_SUFFIX
        assert_eq!(&target[20..32], &DEFAULT_TARGET_SUFFIX);
    }

    #[test]
    fn test_safe_deployment_config_new() {
        let nonce = U256::from(12345);
        let token_amount = U256::from(1000000);
        let admins = vec![address!("0x1234567890123456789012345678901234567890")];

        let config = SafeDeploymentConfig::new(nonce, token_amount, admins.clone());

        assert_eq!(config.nonce, nonce);
        assert_eq!(config.token_amount, token_amount);
        assert_eq!(config.admins, admins);
    }

    #[test]
    fn test_build_user_data() {
        let nonce = U256::from(12345);
        let token_amount = U256::from(1000000);
        let admins = vec![address!("0x1234567890123456789012345678901234567890")];
        
        let config = SafeDeploymentConfig::new(nonce, token_amount, admins);
        
        let network_config = NetworkConfig {
            channels_contract_address: address!("0x693Bac5ce61c720dDC68533991Ceb41199D8F8ae"),
            node_stake_factory_address: address!("0x048D04C9f5F74d65e76626B943779DEC6EdCEFeC"),
        };

        let user_data = config.build_user_data(&network_config);
        
        // User data should not be empty
        assert!(!user_data.is_empty());
        
        // User data should contain the function identifier at the start
        assert_eq!(&user_data[0..32], &DEPLOY_SAFE_MODULE_AND_INCLUDE_NODES_IDENTIFIER);
    }

    #[test]
    fn test_safe_deployment_result() {
        let result = SafeDeploymentResult {
            tx_hash: B256::from([1u8; 32]),
            safe_address: address!("0x1234567890123456789012345678901234567890"),
            module_address: address!("0x0987654321098765432109876543210987654321"),
        };

        assert_eq!(result.tx_hash, B256::from([1u8; 32]));
        assert_eq!(
            result.safe_address,
            address!("0x1234567890123456789012345678901234567890")
        );
        assert_eq!(
            result.module_address,
            address!("0x0987654321098765432109876543210987654321")
        );
    }
}
