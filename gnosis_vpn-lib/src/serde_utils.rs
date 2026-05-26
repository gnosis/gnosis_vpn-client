use std::time::{Duration, SystemTime, UNIX_EPOCH};

use edgli::hopr_lib::api::types::primitive::prelude::{Address, Balance, Currency};
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

pub mod balance {
    use super::*;

    pub fn serialize<C: Currency, S: Serializer>(bal: &Balance<C>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&bal.to_string())
    }

    pub fn deserialize<'de, C: Currency, D: Deserializer<'de>>(d: D) -> Result<Balance<C>, D::Error> {
        let s = String::deserialize(d)?;
        s.parse::<Balance<C>>().map_err(serde::de::Error::custom)
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

pub mod opt_system_time {
    use super::*;

    pub fn serialize<S: Serializer>(t: &Option<SystemTime>, s: S) -> Result<S::Ok, S::Error> {
        match t {
            None => s.serialize_none(),
            Some(t) => {
                let ms = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_millis() as u64;
                s.serialize_some(&ms)
            }
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SystemTime>, D::Error> {
        let ms = Option::<u64>::deserialize(d)?;
        Ok(ms.map(|ms| UNIX_EPOCH + Duration::from_millis(ms)))
    }
}

pub mod duration_ms {
    use super::*;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(d.as_secs_f64() * 1000.0)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let ms = f64::deserialize(d)?;
        Ok(Duration::from_secs_f64(ms / 1000.0))
    }
}

pub mod opt_duration_ms {
    use super::*;

    pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            None => s.serialize_none(),
            Some(d) => s.serialize_some(&(d.as_secs_f64() * 1000.0)),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        let ms = Option::<f64>::deserialize(d)?;
        Ok(ms.map(|ms| Duration::from_secs_f64(ms / 1000.0)))
    }
}
