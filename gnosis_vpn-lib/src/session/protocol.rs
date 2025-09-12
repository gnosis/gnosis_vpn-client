use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Protocol {
    Udp,
    Tcp,
}

impl<'de> Deserialize<'de> for Protocol {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ProtocolVisitor;

        impl<'de> Visitor<'de> for ProtocolVisitor {
            type Value = Protocol;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("case-insensitive string representing a protocol")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                match value.to_lowercase().as_str() {
                    "udp" => Ok(Protocol::Udp),
                    "tcp" => Ok(Protocol::Tcp),
                    _ => Err(de::Error::unknown_variant(value, &["udp", "tcp"])),
                }
            }
        }

        deserializer.deserialize_str(ProtocolVisitor)
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_ref())
    }
}

impl AsRef<str> for Protocol {
    fn as_ref(&self) -> &str {
        match self {
            Protocol::Udp => "udp",
            Protocol::Tcp => "tcp",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Protocol;
    use serde_json;

    #[test]
    fn test_deserialize_udp() {
        let udps = [r#""udp""#, r#""UDP""#, r#""UdP""#];
        for json_data in &udps {
            let protocol: Protocol = serde_json::from_str(json_data).unwrap();
            assert_eq!(protocol, Protocol::Udp);
        }
    }

    #[test]
    fn test_deserialize_tcp() {
        let tcps = [r#""tcp""#, r#""TCP""#, r#""TcP""#];
        for json_data in &tcps {
            let protocol: Protocol = serde_json::from_str(json_data).unwrap();
            assert_eq!(protocol, Protocol::Tcp);
        }
    }

    #[test]
    fn test_deserialize_invalid() {
        let json_data = r#""http""#;
        let result = serde_json::from_str::<Protocol>(json_data);
        assert!(result.is_err());
    }
}
