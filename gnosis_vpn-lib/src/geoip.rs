//! An exit's country, derived from what it announces on-chain instead of what its operator claims.

use maxminddb::Reader;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

/// How the country was derived; the observed address and the tunnel egress are later sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeoSource {
    AnnouncedIp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geo {
    /// ISO 3166-1 alpha-2.
    pub country_code: String,
    pub source: GeoSource,
}

#[derive(Debug, thiserror::Error)]
#[error("cannot open GeoIP database {path}: {source}")]
pub struct OpenError {
    path: PathBuf,
    source: maxminddb::MaxMindDbError,
}

/// The only record key read, so the full IPinfo Lite file and our country-only extract both work.
#[derive(Deserialize)]
struct Record {
    country_code: String,
}

/// A country database held in memory; the shipped file is a few megabytes.
pub struct GeoIp {
    reader: Reader<Vec<u8>>,
}

impl std::fmt::Debug for GeoIp {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let meta = self.reader.metadata();
        write!(f, "GeoIp({} built {})", meta.database_type, meta.build_epoch)
    }
}

impl GeoIp {
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        let reader = Reader::open_readfile(path).map_err(|source| OpenError {
            path: path.to_path_buf(),
            source,
        })?;
        tracing::info!(path = %path.display(), db = %reader.metadata().database_type, build_epoch = reader.metadata().build_epoch, "geoip database loaded");
        Ok(Self { reader })
    }

    /// The country every announced address agrees on; an unknown address or a disagreement yields none.
    pub fn country_of(&self, addrs: &[Ipv4Addr]) -> Option<Geo> {
        let mut agreed: Option<String> = None;
        for addr in addrs {
            let country = self.lookup(*addr)?;
            match &agreed {
                Some(seen) if *seen != country => return None,
                Some(_) => {}
                None => agreed = Some(country),
            }
        }
        agreed.map(|country_code| Geo {
            country_code,
            source: GeoSource::AnnouncedIp,
        })
    }

    fn lookup(&self, addr: Ipv4Addr) -> Option<String> {
        let record = self
            .reader
            .lookup(IpAddr::V4(addr))
            .and_then(|result| result.decode::<Record>())
            .unwrap_or_else(|err| {
                tracing::debug!(%addr, %err, "geoip lookup failed");
                None
            })?;
        Some(record.country_code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> GeoIp {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/geoip-country.mmdb");
        GeoIp::open(&path).expect("fixture opens")
    }

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[test]
    fn a_known_address_resolves_to_its_country() {
        let geo = fixture().country_of(&[ip("185.9.1.73")]).unwrap();

        assert_eq!("US", geo.country_code);
        assert_eq!(GeoSource::AnnouncedIp, geo.source);
    }

    #[test]
    fn neighbours_inside_one_prefix_keep_their_own_country() {
        let geoip = fixture();

        assert_eq!("AU", geoip.country_of(&[ip("185.9.1.9")]).unwrap().country_code);
        assert_eq!("NL", geoip.country_of(&[ip("185.9.1.105")]).unwrap().country_code);
        assert_eq!("SE", geoip.country_of(&[ip("185.9.0.1")]).unwrap().country_code);
    }

    #[test]
    fn agreeing_addresses_resolve_and_disagreeing_ones_do_not() {
        let geoip = fixture();

        assert_eq!(
            "AU",
            geoip
                .country_of(&[ip("185.9.1.9"), ip("185.9.1.1")])
                .unwrap()
                .country_code
        );
        assert_eq!(None, geoip.country_of(&[ip("185.9.1.9"), ip("185.9.1.73")]));
    }

    #[test]
    fn an_unknown_address_yields_no_country_even_beside_a_known_one() {
        let geoip = fixture();

        assert_eq!(None, geoip.country_of(&[ip("8.8.8.8")]));
        assert_eq!(None, geoip.country_of(&[ip("185.9.1.9"), ip("8.8.8.8")]));
        assert_eq!(None, geoip.country_of(&[]));
    }

    #[test]
    fn a_missing_database_is_an_open_error_naming_the_path() {
        let err = GeoIp::open(Path::new("/nonexistent/geoip.mmdb")).unwrap_err();

        assert!(err.to_string().contains("/nonexistent/geoip.mmdb"), "{err}");
    }

    #[test]
    fn geo_serializes_with_a_snake_case_source() {
        let geo = Geo {
            country_code: "GB".to_string(),
            source: GeoSource::AnnouncedIp,
        };

        assert_eq!(
            r#"{"country_code":"GB","source":"announced_ip"}"#,
            serde_json::to_string(&geo).unwrap()
        );
    }
}
