// mostly copied from hopr-lib

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::fmt::{self, Display};
use std::str::FromStr;

/// Represents an Ethereum address
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default, Hash, PartialOrd, Ord)]
pub struct Address([u8; 20]);

#[derive(Debug, Error)]
pub enum Error {
    #[error("Error in hex represantation: {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("Invalid length, expected 20 bytes, got {0}")]
    InvalidLength(usize),
    #[error("Invalid address format")]
    InvalidFormat,
    #[error("Address conversion failed")]
    ConversionFailed,
}

impl Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

impl Serialize for Address {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Address {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AddressVisitor;

        impl<'de> Visitor<'de> for AddressVisitor {
            type Value = Address;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("hex string representing an Ethereum address")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let address = Address::from_hex(value).map_err(de::Error::custom)?;
                Ok(address)
            }
        }

        deserializer.deserialize_str(AddressVisitor)
    }
}

impl Address {
    pub fn new(bytes: [u8; 20]) -> Self {
        Address(bytes)
    }

    pub fn from_hex(str: &str) -> Result<Self, Error> {
        if str.is_empty() || str.len() % 2 != 0 {
            return Err(Error::InvalidFormat);
        }

        let data = if &str[..2] == "0x" || &str[..2] == "0X" {
            &str[2..]
        } else {
            str
        };
        let bytes: Vec<u8> = hex::decode(data)?;
        if bytes.len() != 20 {
            return Err(Error::InvalidLength(bytes.len()));
        }
        let array: [u8; 20] = bytes.try_into().map_err(|_| Error::ConversionFailed)?;
        Ok(Address(array))
    }
}

impl FromStr for Address {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s)
    }
}
