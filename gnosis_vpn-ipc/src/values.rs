//! Value types that carry their own validation, so a parsed message is a valid message.
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha3::{Digest, Keccak256};

use std::fmt::{self, Display};
use std::iter::Sum;
use std::marker::PhantomData;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("not an address: {0}")]
    Address(String),
    #[error("address checksum mismatch: {0}")]
    AddressChecksum(String),
    #[error("not a wei amount: {0}")]
    Balance(String),
    #[error("hop count {0} exceeds {max}", max = Hops::MAX)]
    Hops(u8),
    #[error("not a WireGuard public key: {0}")]
    WgPublicKey(String),
    #[error("not an ISO 3166-1 alpha-2 country code: {0}")]
    CountryCode(String),
    #[error("not an API version label: {0}")]
    ApiVersion(String),
}

/// An Ethereum address; EIP-55 checksum on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address([u8; 20]);

impl Address {
    pub const fn new(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    pub fn bytes(&self) -> [u8; 20] {
        self.0
    }

    /// `0x1234..abcd`, for log lines and dense terminal output.
    pub fn short(&self) -> String {
        let full = self.to_string();
        format!("{}..{}", &full[..6], &full[38..])
    }
}

impl Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let lower = hex_lower(&self.0);
        let hash = Keccak256::digest(lower.as_bytes());
        let mut out = String::with_capacity(42);
        out.push_str("0x");
        for (i, c) in lower.chars().enumerate() {
            let nibble = (hash[i / 2] >> if i % 2 == 0 { 4 } else { 0 }) & 0x0f;
            let is_upper = c.is_ascii_alphabetic() && nibble >= 8;
            out.push(if is_upper { c.to_ascii_uppercase() } else { c });
        }
        f.write_str(&out)
    }
}

impl FromStr for Address {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s
            .strip_prefix("0x")
            .filter(|h| h.len() == 40)
            .ok_or_else(|| ParseError::Address(s.to_string()))?;
        let mut bytes = [0u8; 20];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| ParseError::Address(s.to_string()))?;
        }
        let address = Address(bytes);
        // EIP-55: single-case input carries no checksum, mixed case must match it.
        let is_mixed_case = hex.chars().any(|c| c.is_ascii_lowercase()) && hex.chars().any(|c| c.is_ascii_uppercase());
        if is_mixed_case && address.to_string() != s {
            return Err(ParseError::AddressChecksum(s.to_string()));
        }
        Ok(address)
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub trait Currency {
    const NAME: &'static str;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WxHopr {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XDai {}

impl Currency for WxHopr {
    const NAME: &'static str = "wxHOPR";
}

impl Currency for XDai {
    const NAME: &'static str = "xDai";
}

/// Both tokens have 18 decimals.
const WEI_PER_TOKEN: u128 = 1_000_000_000_000_000_000;

/// Token amounts below this are hard to read as decimals and are also shown in scientific notation.
const SCIENTIFIC_BELOW_TOKENS: f64 = 1e-3;

/// Wei, typed by currency so tokens never mix; a decimal string on the wire as JSON numbers stop at 2^53.
pub struct Balance<C> {
    wei: u128,
    currency: PhantomData<C>,
}

impl<C> Balance<C> {
    pub const fn from_wei(wei: u128) -> Self {
        Self {
            wei,
            currency: PhantomData,
        }
    }

    pub const fn zero() -> Self {
        Self::from_wei(0)
    }

    pub fn wei(&self) -> u128 {
        self.wei
    }

    pub fn is_zero(&self) -> bool {
        self.wei == 0
    }

    /// `7.5e-10` for amounts too small to read as decimals, `None` otherwise.
    pub fn scientific(&self) -> Option<String> {
        let tokens = self.wei as f64 / WEI_PER_TOKEN as f64;
        let is_tiny = tokens > 0.0 && tokens < SCIENTIFIC_BELOW_TOKENS;
        if !is_tiny {
            return None;
        }
        let formatted = format!("{tokens:.2e}");
        let (mantissa, exponent) = formatted.split_once('e')?;
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        Some(format!("{mantissa}e{exponent}"))
    }
}

// Manual impls: derives would wrongly require the currency marker to implement them.
impl<C> Clone for Balance<C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for Balance<C> {}

impl<C> PartialEq for Balance<C> {
    fn eq(&self, other: &Self) -> bool {
        self.wei == other.wei
    }
}

impl<C> Eq for Balance<C> {}

impl<C> PartialOrd for Balance<C> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<C> Ord for Balance<C> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.wei.cmp(&other.wei)
    }
}

impl<C> Default for Balance<C> {
    fn default() -> Self {
        Self::zero()
    }
}

impl<C: Currency> fmt::Debug for Balance<C> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl<C> Sum for Balance<C> {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        Self::from_wei(iter.map(|b| b.wei).fold(0, u128::saturating_add))
    }
}

/// `1.5 wxHOPR`: whole tokens with trailing zeros trimmed.
impl<C: Currency> Display for Balance<C> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let whole = self.wei / WEI_PER_TOKEN;
        let fraction = self.wei % WEI_PER_TOKEN;
        if fraction == 0 {
            return write!(f, "{whole} {}", C::NAME);
        }
        let digits = format!("{fraction:018}");
        write!(f, "{whole}.{} {}", digits.trim_end_matches('0'), C::NAME)
    }
}

impl<C> Serialize for Balance<C> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&self.wei)
    }
}

impl<'de, C> Deserialize<'de> for Balance<C> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        let wei = raw
            .parse::<u128>()
            .map_err(|_| serde::de::Error::custom(ParseError::Balance(raw.clone())))?;
        Ok(Self::from_wei(wei))
    }
}

/// Intermediate relays on the way to an exit; anything above `MAX` cannot be routed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Hops(u8);

impl Hops {
    pub const MAX: u8 = 3;

    pub fn count(&self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Hops {
    type Error = ParseError;

    fn try_from(count: u8) -> Result<Self, Self::Error> {
        if count > Self::MAX {
            return Err(ParseError::Hops(count));
        }
        Ok(Self(count))
    }
}

impl From<Hops> for u8 {
    fn from(hops: Hops) -> Self {
        hops.0
    }
}

impl Display for Hops {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

macro_rules! string_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_newtype!(
    /// The handle `status` shows and `connect` accepts.
    DestinationId
);
string_newtype!(SessionId);
string_newtype!(
    /// A libp2p peer id, kept as text so the protocol needs no libp2p.
    PeerId
);

/// The funding tool's one-time secret; never printed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FundingSecret(String);

impl FundingSecret {
    pub fn new(secret: String) -> Self {
        Self(secret)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for FundingSecret {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("FundingSecret(***)")
    }
}

/// A Curve25519 public key; base64 on the wire, as `wg` prints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WgPublicKey([u8; 32]);

impl WgPublicKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl Display for WgPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&BASE64.encode(self.0))
    }
}

impl FromStr for WgPublicKey {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let decoded = BASE64.decode(s).map_err(|_| ParseError::WgPublicKey(s.to_string()))?;
        let bytes: [u8; 32] = decoded.try_into().map_err(|_| ParseError::WgPublicKey(s.to_string()))?;
        Ok(Self(bytes))
    }
}

/// Upper-case ISO 3166-1 alpha-2, e.g. `DE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CountryCode([u8; 2]);

impl Display for CountryCode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}{}", self.0[0] as char, self.0[1] as char)
    }
}

impl FromStr for CountryCode {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes: [u8; 2] = s
            .as_bytes()
            .try_into()
            .map_err(|_| ParseError::CountryCode(s.to_string()))?;
        if !bytes.iter().all(u8::is_ascii_alphabetic) {
            return Err(ParseError::CountryCode(s.to_string()));
        }
        Ok(Self(bytes.map(|b| b.to_ascii_uppercase())))
    }
}

/// An exit server's API label; `v1` on the wire, the same as `gnosis_vpn-server` publishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ApiVersion(u32);

impl ApiVersion {
    pub const fn new(major: u32) -> Self {
        Self(major)
    }

    /// Where this version's routes live on the exit server.
    pub fn path(&self) -> String {
        format!("/api/{self}")
    }
}

impl Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

impl FromStr for ApiVersion {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.strip_prefix('v')
            .and_then(|n| n.parse::<u32>().ok())
            .map(Self)
            .ok_or_else(|| ParseError::ApiVersion(s.to_string()))
    }
}

/// Types whose wire form is their `Display`/`FromStr` text.
macro_rules! serde_via_str {
    ($($name:ty),*) => {$(
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(d)?;
                raw.parse().map_err(serde::de::Error::custom)
            }
        }
    )*};
}

serde_via_str!(Address, WgPublicKey, CountryCode, ApiVersion);

/// Unix milliseconds on the wire.
pub mod unix_ms {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(to_ms(t))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        Ok(UNIX_EPOCH + Duration::from_millis(u64::deserialize(d)?))
    }

    pub(crate) fn to_ms(t: &SystemTime) -> u64 {
        let since_epoch = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
        u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
    }

    pub mod option {
        use super::*;

        pub fn serialize<S: Serializer>(t: &Option<SystemTime>, s: S) -> Result<S::Ok, S::Error> {
            t.as_ref().map(to_ms).serialize(s)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SystemTime>, D::Error> {
            Ok(Option::<u64>::deserialize(d)?.map(|ms| UNIX_EPOCH + Duration::from_millis(ms)))
        }
    }
}

/// Whole milliseconds on the wire.
pub mod duration_ms {
    use super::*;
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(to_ms(d))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_millis(u64::deserialize(d)?))
    }

    fn to_ms(d: &Duration) -> u64 {
        u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
    }

    pub mod option {
        use super::*;

        pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
            d.as_ref().map(to_ms).serialize(s)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
            Ok(Option::<u64>::deserialize(d)?.map(Duration::from_millis))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vectors from EIP-55.
    const CHECKSUMMED: [&str; 2] = [
        "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
        "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
    ];

    #[test]
    fn address_round_trips_through_its_checksum() {
        for s in CHECKSUMMED {
            let address: Address = s.parse().expect("valid address");
            assert_eq!(address.to_string(), s);
        }
    }

    #[test]
    fn address_accepts_single_case_input() {
        let lower: Address = CHECKSUMMED[0].to_lowercase().parse().expect("lower case");
        assert_eq!(lower.to_string(), CHECKSUMMED[0]);
    }

    #[test]
    fn address_rejects_a_wrong_checksum() {
        let wrong = CHECKSUMMED[0].replace("aAeb", "AAeb");
        assert_eq!(
            wrong.parse::<Address>(),
            Err(ParseError::AddressChecksum(wrong.clone()))
        );
    }

    #[test]
    fn address_rejects_wrong_length_and_prefix() {
        assert!("0x1234".parse::<Address>().is_err());
        assert!(CHECKSUMMED[0].trim_start_matches("0x").parse::<Address>().is_err());
    }

    #[test]
    fn balance_is_a_wei_string_on_the_wire() {
        let balance = Balance::<WxHopr>::from_wei(1_500_000_000_000_000_000);
        let json = serde_json::to_string(&balance).expect("serialize");
        assert_eq!(json, r#""1500000000000000000""#);
        let back: Balance<WxHopr> = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, balance);
    }

    #[test]
    fn balance_rejects_token_notation() {
        assert!(serde_json::from_str::<Balance<XDai>>(r#""1.5 xDai""#).is_err());
    }

    #[test]
    fn balance_displays_whole_tokens() {
        assert_eq!(
            Balance::<WxHopr>::from_wei(1_500_000_000_000_000_000).to_string(),
            "1.5 wxHOPR"
        );
        assert_eq!(Balance::<XDai>::from_wei(2 * WEI_PER_TOKEN).to_string(), "2 xDai");
        assert_eq!(Balance::<XDai>::zero().to_string(), "0 xDai");
        assert_eq!(
            Balance::<WxHopr>::from_wei(1).to_string(),
            "0.000000000000000001 wxHOPR"
        );
    }

    #[test]
    fn balance_scientific_only_for_tiny_amounts() {
        assert_eq!(
            Balance::<WxHopr>::from_wei(750_000_000).scientific().as_deref(),
            Some("7.5e-10")
        );
        assert_eq!(Balance::<WxHopr>::from_wei(1).scientific().as_deref(), Some("1e-18"));
        assert_eq!(Balance::<WxHopr>::zero().scientific(), None);
        assert_eq!(Balance::<WxHopr>::from_wei(WEI_PER_TOKEN).scientific(), None);
    }

    #[test]
    fn hops_reject_more_than_max() {
        assert!(serde_json::from_str::<Hops>("3").is_ok());
        assert!(serde_json::from_str::<Hops>("4").is_err());
    }

    #[test]
    fn wg_public_key_is_32_bytes_of_base64() {
        let key = BASE64.encode([7u8; 32]);
        assert_eq!(key.parse::<WgPublicKey>().expect("valid key").to_string(), key);
        assert!(BASE64.encode([7u8; 31]).parse::<WgPublicKey>().is_err());
    }

    #[test]
    fn country_code_is_two_upper_case_letters() {
        assert_eq!("de".parse::<CountryCode>().expect("valid").to_string(), "DE");
        assert!("DEU".parse::<CountryCode>().is_err());
        assert!("D1".parse::<CountryCode>().is_err());
    }

    #[test]
    fn api_version_is_a_v_label() {
        let v1: ApiVersion = "v1".parse().expect("valid label");
        assert_eq!(v1, ApiVersion::new(1));
        assert_eq!(v1.path(), "/api/v1");
        assert!("v1.0".parse::<ApiVersion>().is_err());
        assert!("1".parse::<ApiVersion>().is_err());
    }

    #[test]
    fn funding_secret_never_prints() {
        let secret = FundingSecret::new("hunter2".to_string());
        assert!(!format!("{secret:?}").contains("hunter2"));
    }
}
