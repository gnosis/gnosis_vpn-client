use edgli::hopr_chain_connector::reexports::alloy::{
    primitives::{Address, B256, Bytes, U256, address},
    providers::Provider,
    sol,
    sol_types::SolType,
};
use edgli::hopr_lib::{Address as HoprAddress, Balance, WxHOPR};
use edgli::hopr_lib::{EncodedWinProb, WinningProbability};
use primitive_types::U256 as PrimitiveU256;

use crate::{
    chain::{
        client::GnosisProvider,
        constants::{DEFAULT_TARGET_SUFFIX, DEPLOY_SAFE_MODULE_AND_INCLUDE_NODES_IDENTIFIER, WXHOPR_TOKEN_ADDRESS},
        errors::ChainError,
    },
    network::Network,
    ticket_stats::TicketStats,
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

sol!(
    #[sol(rpc)]
    contract HoprWinningProbabilityOracle {
        function currentWinProb() external view returns (uint56);
    }
);

sol!(
    #[sol(rpc)]
    contract HoprTicketPriceOracle {
        function currentTicketPrice() external view returns (uint256);
    }
);

type UserDataTuple = sol! { tuple(bytes32, uint256, bytes32, address[]) };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkContracts {
    pub channels_contract_address: Address,
    pub node_stake_factory_address: Address,
    pub win_prob_oracle_address: Address,
    pub token_price_oracle_address: Address,
}

impl NetworkContracts {
    /// Build the default target as bytes32 by concatenating channel contract address with DEFAULT_TARGET_SUFFIX
    pub fn build_default_target(&self) -> B256 {
        let channels_address = self.channels_contract_address;

        let mut target_bytes = [0u8; 32];

        // Copy the 20 bytes of the address
        target_bytes[0..20].copy_from_slice(channels_address.as_slice());

        // Copy the 12 bytes of the suffix (DEFAULT_TARGET_SUFFIX is exactly 12 bytes)
        target_bytes[20..32].copy_from_slice(&DEFAULT_TARGET_SUFFIX);

        B256::from(target_bytes)
    }

    pub async fn get_win_prob_ticket_price(&self, provider: &GnosisProvider) -> Result<TicketStats, ChainError> {
        let win_prob_oracle_instance =
            HoprWinningProbabilityOracle::new(self.win_prob_oracle_address, provider.clone());
        let ticket_price_oracle_instance =
            HoprTicketPriceOracle::new(self.token_price_oracle_address, provider.clone());

        let multicall = provider
            .multicall()
            .add(win_prob_oracle_instance.currentWinProb())
            .add(ticket_price_oracle_instance.currentTicketPrice());

        let (win_prob_raw, ticket_price_raw) = multicall.aggregate().await?;

        // convert win_prob from u56 to f64
        let mut encoded: EncodedWinProb = Default::default();
        encoded.copy_from_slice(&win_prob_raw.to_be_bytes_vec());
        let current_win_prob = WinningProbability::from(encoded).as_f64();
        let ticket_price_bytes = ticket_price_raw.to_be_bytes::<32>();
        let ticket_price_u256 = PrimitiveU256::from_big_endian(&ticket_price_bytes);
        let ticket_price = Balance::<WxHOPR>::from(ticket_price_u256);
        Ok(TicketStats::new(ticket_price, current_win_prob))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetworkSpecifications {
    pub network: Network,
    pub contracts: NetworkContracts,
}

impl NetworkSpecifications {
    pub fn from_network(network: &Network) -> Self {
        let contracts = match network {
            Network::Dufour => NetworkContracts {
                channels_contract_address: address!("0x693Bac5ce61c720dDC68533991Ceb41199D8F8ae"),
                node_stake_factory_address: address!("0x048D04C9f5F74d65e76626B943779DEC6EdCEFeC"),
                win_prob_oracle_address: address!("0x7Eb8d762fe794A108e568aD2097562cc5D3A1359"),
                token_price_oracle_address: address!("0xcA5656Fe6F2d847ACA32cf5f38E51D2054cA1273"),
            },
            Network::Rotsee => NetworkContracts {
                channels_contract_address: address!("0x77C9414043d27fdC98A6A2d73fc77b9b383092a7"),
                node_stake_factory_address: address!("0x439f5457FF58CEE941F7d946CB919c52EA30cfB3"),
                win_prob_oracle_address: address!("0xC15675d4CCa538D91a91a8D3EcFBB8499C3B0471"),
                token_price_oracle_address: address!("0x624af123A0149670848FA95e972b35FFeE6A48Fb"),
            },
        };
        Self {
            network: network.clone(),
            contracts,
        }
    }

    pub fn get_network_specification(network: &Network) -> NetworkSpecifications {
        NetworkSpecifications::from_network(network)
    }

    pub fn get_network_contracts(network: &Network) -> NetworkContracts {
        Self::from_network(network).contracts
    }
}

#[derive(Clone, Debug)]
pub struct SafeModuleDeploymentInputs {
    pub token_amount: U256,
    pub nonce: U256,
    pub admins: Vec<Address>,
}

#[derive(Clone, Debug)]
pub struct SafeModuleDeploymentResult {
    pub safe_address: Address,
    pub module_address: Address,
}

impl SafeModuleDeploymentResult {
    pub fn new(safe_address: HoprAddress, module_address: HoprAddress) -> Self {
        Self {
            safe_address: Address::try_from(safe_address.as_ref()).unwrap(),
            module_address: Address::try_from(module_address.as_ref()).unwrap(),
        }
    }
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
    pub fn build_user_data(&self, network: &Network) -> Bytes {
        let default_target = NetworkSpecifications::get_network_contracts(network).build_default_target();

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

    pub async fn deploy(
        &self,
        provider: &GnosisProvider,
        network: Network,
    ) -> Result<SafeModuleDeploymentResult, ChainError> {
        let token_instance = Token::new(WXHOPR_TOKEN_ADDRESS, provider.clone());
        // Implementation for deploying the safe module using the client
        let user_data = self.build_user_data(&network);

        // deploy the safe module by calling send on the wxHOPR token contract
        let pending_tx = token_instance
            .send(
                NetworkSpecifications::get_network_contracts(&network).node_stake_factory_address,
                self.token_amount,
                user_data,
            )
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
            safe_address: safe_log.instance,
            module_address: module_log.instance,
        })
    }
}

#[derive(Clone, Debug)]
pub struct CheckBalanceInputs {
    pub hopr_token_holder: Address,
    pub native_token_holder: Address,
}

#[derive(Clone, Debug)]
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

/// Send HOPR tokens to a recipient address
pub async fn send_hopr_tokens(provider: &GnosisProvider, recipient: Address, amount: U256) -> Result<B256, ChainError> {
    let token_instance = Token::new(WXHOPR_TOKEN_ADDRESS, provider.clone());
    let pending_tx = token_instance.send(recipient, amount, Bytes::new()).send().await?;
    let receipt = pending_tx.get_receipt().await?;

    Ok(receipt.transaction_hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use edgli::hopr_chain_connector::reexports::alloy::primitives::{U256, address, hex};

    #[test]
    fn build_user_data_encodes_nonce_token_amount_and_admins() -> anyhow::Result<()> {
        let nonce = U256::from(999);
        let token_amount = U256::from(500000000000000000u64); // 0.5 tokens
        let admins = vec![
            address!("0x1111111111111111111111111111111111111111"),
            address!("0x2222222222222222222222222222222222222222"),
        ];
        let network = Network::Rotsee;

        let inputs = SafeModuleDeploymentInputs::new(nonce, token_amount, admins);
        let user_data = inputs.build_user_data(&network);

        // Verify the data is not empty
        assert_eq!(
            user_data.as_ref(),
            hex!(
                "0105b97dcdf19d454ebe36f91ed516c2b90ee79f4a46af96a0138c1f5403c1cc00000000000000000000000000000000000000000000000000000000000003e777c9414043d27fdc98a6a2d73fc77b9b383092a70101030202020202020202020000000000000000000000000000000000000000000000000000000000000080000000000000000000000000000000000000000000000000000000000000000200000000000000000000000011111111111111111111111111111111111111110000000000000000000000002222222222222222222222222222222222222222"
            )
        );
        Ok(())
    }

    #[test]
    fn network_specifications_return_dufour_defaults() -> anyhow::Result<()> {
        let dufour_spec = NetworkSpecifications::get_network_specification(&Network::Dufour);
        assert_eq!(dufour_spec.network, Network::Dufour);
        Ok(())
    }

    #[test]
    fn build_default_target_matches_known_rotsee_bytes() -> anyhow::Result<()> {
        let default_target = NetworkSpecifications::get_network_contracts(&Network::Rotsee).build_default_target();
        let used_default_target = hex!("77c9414043d27fdc98a6a2d73fc77b9b383092a7010103020202020202020202");
        assert_eq!(default_target, used_default_target);
        Ok(())
    }
}
