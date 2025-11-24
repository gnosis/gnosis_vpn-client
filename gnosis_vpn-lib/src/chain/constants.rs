use edgli::hopr_lib::reexports::alloy::primitives::{Address, address, hex};

// wxHOPR Token contract address on Gnosis Chain
pub const WXHOPR_TOKEN_ADDRESS: Address = address!("0xD4fdec44DB9D44B8f2b6d529620f9C0C7066A2c1");
// Default target suffix to be appended to Channels contract address
pub const DEFAULT_TARGET_SUFFIX: [u8; 12] = hex!("010103020202020202020202");

pub const DEPLOY_SAFE_MODULE_AND_INCLUDE_NODES_IDENTIFIER: [u8; 32] =
    hex!("0105b97dcdf19d454ebe36f91ed516c2b90ee79f4a46af96a0138c1f5403c1cc");
