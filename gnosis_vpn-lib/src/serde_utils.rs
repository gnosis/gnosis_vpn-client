use edgli::hopr_lib::api::types::primitive::prelude::Address;
use serde::{Deserialize, Deserializer, Serializer};

pub mod address {
    use super::*;

    pub fn serialize<S: Serializer>(addr: &Address, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&addr.to_checksum())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Address, D::Error> {
        let hex = String::deserialize(d)?;
        hex.parse::<Address>().map_err(serde::de::Error::custom)
    }
}
