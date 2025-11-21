use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};
use std::str::FromStr;

#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Network {
    Rotsee,
    #[default]
    Dufour,
}

impl Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let network_str = match self {
            Network::Rotsee => "rotsee",
            Network::Dufour => "dufour",
        };
        write!(f, "{}", network_str)
    }
}

impl FromStr for Network {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "rotsee" => Ok(Network::Rotsee),
            "dufour" => Ok(Network::Dufour),
            other => Err(format!("unknown network '{}', expected 'rotsee' or 'dufour'", other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_accepts_known_network_names_case_insensitively() -> anyhow::Result<()> {
        assert_eq!(
            "Rotsee".parse::<Network>().expect("rotsee should parse"),
            Network::Rotsee
        );
        assert_eq!(
            "DUFOUR".parse::<Network>().expect("dufour should parse"),
            Network::Dufour
        );

        Ok(())
    }

    #[test]
    fn from_str_rejects_unknown_network_names() -> anyhow::Result<()> {
        "unknown"
            .parse::<Network>()
            .expect_err("unknown network names should return an error");

        Ok(())
    }
}
