use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

pub mod system_time {
    use super::*;

    // u64 milliseconds covers ~584 million years from UNIX_EPOCH, which is
    // sufficient for any real timestamp. Using u64 avoids the need for u128,
    // which serde_json doesn't natively support.
    pub fn serialize<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        let ms = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_millis() as u64;
        s.serialize_u64(ms)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let ms = u64::deserialize(d)?;
        Ok(UNIX_EPOCH + Duration::from_millis(ms))
    }
}
