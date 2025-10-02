use alloy::{
    primitives::{Address, B256, Bytes, U256},
    providers::Provider,
    sol,
    sol_types::SolType,
};

use crate::chain::{
    client::GnosisProvider,
    constants::{
        CHANNELS_CONTRACT_ADDRESS, DEFAULT_TARGET_SUFFIX, DEPLOY_SAFE_MODULE_AND_INCLUDE_NODES_IDENTIFIER,
        NODE_STAKE_FACTORY_ADDRESS, WXHOPR_TOKEN_ADDRESS,
    },
    errors::ChainError,
};

// Interface for send() function of wxHOPR token contract
sol! {
    #[sol(rpc)]
    contract Token {
        function send(address recipient, uint256 amount, bytes memory data) external;
        function balanceOf(address account) external view returns (uint256);
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

/// Build the default target as bytes32 by concatenating CHANNELS_CONTRACT_ADDRESS with DEFAULT_TARGET_SUFFIX
pub fn build_default_target() -> B256 {
    let mut target_bytes = [0u8; 32];

    // Copy the 20 bytes of the address
    target_bytes[0..20].copy_from_slice(CHANNELS_CONTRACT_ADDRESS.as_slice());

    // Copy the 12 bytes of the suffix (DEFAULT_TARGET_SUFFIX is exactly 12 bytes)
    target_bytes[20..32].copy_from_slice(&DEFAULT_TARGET_SUFFIX);

    B256::from(target_bytes)
}

pub struct SafeModuleDeploymentInputs {
    pub token_amount: U256,
    pub nonce: U256,
    pub admins: Vec<Address>,
}

pub struct SafeModuleDeploymentResult {
    pub tx_hash: B256,
    pub safe_address: Address,
    pub module_address: Address,
}

impl SafeModuleDeploymentInputs {
    pub fn new(nonce: U256, token_amount: U256, admins: Vec<Address>) -> Self {
        Self {
            nonce,
            token_amount,
            admins,
        }
    }

    /// Build user data equivalent to Solidity:
    /// `abi.encode(factory.DEPLOYSAFEMODULE_FUNCTION_IDENTIFIER(), nonce, DEFAULT_TARGET, admins)`
    /// Where:
    /// - DEPLOYSAFEMODULE_FUNCTION_IDENTIFIER = DEPLOY_SAFE_MODULE_AND_INCLUDE_NODES_IDENTIFIER
    /// - DEFAULT_TARGET = CHANNELS_CONTRACT_ADDRESS + DEFAULT_TARGET_SUFFIX as bytes32
    pub fn build_user_data(&self) -> Bytes {
        let default_target = build_default_target();

        let user_data_with_offset = UserDataTuple::abi_encode(&(
            DEPLOY_SAFE_MODULE_AND_INCLUDE_NODES_IDENTIFIER,
            self.nonce,
            default_target,
            self.admins.clone(),
        ));

        // remove the first 32 bytes which is the offset
        let user_data = user_data_with_offset[32..].to_vec();
        Bytes::from(user_data)
    }

    pub async fn deploy(&self, provider: &GnosisProvider) -> Result<SafeModuleDeploymentResult, ChainError> {
        let token_instance = Token::new(WXHOPR_TOKEN_ADDRESS, provider.clone());
        // Implementation for deploying the safe module using the client
        let user_data = self.build_user_data();

        // deploy the safe module by calling send on the wxHOPR token contract
        let pending_tx = token_instance
            .send(NODE_STAKE_FACTORY_ADDRESS, self.token_amount, user_data)
            .send()
            .await?;

        let receipt = pending_tx.get_receipt().await?;
        let maybe_safe_log = receipt.decoded_log::<HoprNodeStakeFactory::NewHoprNodeStakeSafe>();
        let Some(safe_log) = maybe_safe_log else {
            return Err(ChainError::DecodeEventError("NewHoprNodeStakeSafe".to_string()));
        };
        let maybe_module_log = receipt.decoded_log::<HoprNodeStakeFactory::NewHoprNodeStakeModule>();
        let Some(module_log) = maybe_module_log else {
            return Err(ChainError::DecodeEventError("NewHoprNodeStakeModule".to_string()));
        };

        Ok(SafeModuleDeploymentResult {
            tx_hash: receipt.transaction_hash,
            safe_address: safe_log.instance,
            module_address: module_log.instance,
        })
    }
}

pub struct CheckBalanceInputs {
    pub hopr_token_holder: Address,
    pub native_token_holder: Address,
}

pub struct CheckBalanceResult {
    pub hopr_token_balance: U256,
    pub native_token_balance: U256,
}

impl CheckBalanceInputs {
    pub fn new(hopr_token_holder: Address, native_token_holder: Address) -> Self {
        Self {
            hopr_token_holder,
            native_token_holder,
        }
    }

    pub async fn check(&self, provider: &GnosisProvider) -> Result<CheckBalanceResult, ChainError> {
        let token_instance = Token::new(WXHOPR_TOKEN_ADDRESS, provider.clone());

        let multicall = provider
            .multicall()
            .add(token_instance.balanceOf(self.hopr_token_holder))
            .get_eth_balance(self.native_token_holder);

        let (hopr_token_balance, native_token_balance) = multicall.aggregate().await?;

        Ok(CheckBalanceResult {
            hopr_token_balance,
            native_token_balance,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, hex, uint};

    #[test]
    fn test_build_default_target() {
        let default_target = build_default_target();
        let used_default_target = hex!("77c9414043d27fdc98a6a2d73fc77b9b383092a7010103020202020202020202");
        assert_eq!(default_target, used_default_target);
    }

    #[test]
    fn test_safe_module_deployment_user_data_encoding() {
        let nonce = uint!(999_U256);
        let token_amount = uint!(500000000000000000_U256); // 0.5 tokens
        let admins = vec![
            address!("0x1111111111111111111111111111111111111111"),
            address!("0x2222222222222222222222222222222222222222"),
        ];

        let inputs = SafeModuleDeploymentInputs::new(nonce, token_amount, admins);
        let user_data = inputs.build_user_data();

        // Verify the data is not empty
        assert_eq!(
            user_data.as_ref(),
            hex!(
                "0105b97dcdf19d454ebe36f91ed516c2b90ee79f4a46af96a0138c1f5403c1cc00000000000000000000000000000000000000000000000000000000000003e777c9414043d27fdc98a6a2d73fc77b9b383092a70101030202020202020202020000000000000000000000000000000000000000000000000000000000000080000000000000000000000000000000000000000000000000000000000000000200000000000000000000000011111111111111111111111111111111111111110000000000000000000000002222222222222222222222222222222222222222"
            )
        );
    }
}
