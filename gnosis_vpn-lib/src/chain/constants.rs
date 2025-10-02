use alloy::primitives::{Address, address, hex};

// wxHOPR Token contract address on Gnosis Chain
pub const WXHOPR_TOKEN_ADDRESS: Address = address!("0xD4fdec44DB9D44B8f2b6d529620f9C0C7066A2c1");
// Channels contract address in Rotsee network on Gnosis Chain. NOTE: This contract is network dependent.
pub const CHANNELS_CONTRACT_ADDRESS: Address = address!("0x77C9414043d27fdC98A6A2d73fc77b9b383092a7");
// NodeStakeFactory contract address in Rotsee network on Gnosis Chain. NOTE: This contract is network dependent.
pub const NODE_STAKE_FACTORY_ADDRESS: Address = address!("0x439f5457FF58CEE941F7d946CB919c52EA30cfB3");
// Default target suffix to be appended to Channels contract address
pub const DEFAULT_TARGET_SUFFIX: [u8; 12] = hex!("010103020202020202020202");

pub const DEPLOY_SAFE_MODULE_AND_INCLUDE_NODES_IDENTIFIER: [u8; 32] =
    hex!("0105b97dcdf19d454ebe36f91ed516c2b90ee79f4a46af96a0138c1f5403c1cc");
