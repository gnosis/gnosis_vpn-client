use edgli::hopr_lib::HoprKeys;
use edgli::hopr_lib::config::HoprLibConfig;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use url::Url;

use std::path::PathBuf;

use crate::hopr::{config, identity};

#[derive(Debug, Error)]
pub enum Error {
    #[error("HOPR identity error: {0}")]
    HoprIdentity(#[from] identity::Error),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("HOPR config error: {0}")]
    Config(#[from] config::Error),
    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HoprParams {
    identity_file: Option<PathBuf>,
    identity_pass: Option<String>,
    config_mode: ConfigFileMode,
    allow_insecure: bool,
    blokli_url: Option<Url>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ConfigFileMode {
    Manual(PathBuf),
    Generated,
}

impl HoprParams {
    pub fn new(
        identity_file: Option<PathBuf>,
        identity_pass: Option<String>,
        config_mode: ConfigFileMode,
        allow_insecure: bool,
        blokli_url: Option<Url>,
    ) -> Self {
        Self {
            identity_file,
            identity_pass,
            config_mode,
            allow_insecure,
            blokli_url,
        }
    }

    pub async fn persist_identity_generation(&self) -> Result<HoprKeys, Error> {
        let identity_file = match &self.identity_file {
            Some(path) => {
                tracing::info!(?path, "Using provided HOPR identity file");
                path.to_path_buf()
            }
            None => identity::file()?,
        };

        let identity_pass = match &self.identity_pass {
            Some(pass) => {
                tracing::info!("Using provided HOPR identity pass");
                pass.to_string()
            }
            None => {
                let path = identity::pass_file()?;
                match fs::read_to_string(&path).await {
                    Ok(p) => {
                        tracing::debug!(?path, "No HOPR identity pass provided - read from file instead");
                        Ok(p)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        tracing::debug!(
                            ?path,
                            "No HOPR identity pass provided - generating new one and storing alongside identity file"
                        );
                        let pw = identity::generate_pass();
                        fs::write(&path, pw.as_bytes()).await?;
                        Ok(pw)
                    }
                    Err(e) => Err(e),
                }?
            }
        };

        identity::from_path(identity_file.as_path(), identity_pass.clone()).map_err(Error::from)
    }

    pub async fn calc_keys(&self) -> Result<HoprKeys, Error> {
        let identity_file = match &self.identity_file {
            Some(path) => path.to_path_buf(),
            None => identity::file()?,
        };

        let identity_pass = match &self.identity_pass {
            Some(pass) => pass.to_string(),
            None => {
                let path = identity::pass_file()?;
                fs::read_to_string(&path).await?
            }
        };

        identity::from_path(identity_file.as_path(), identity_pass.clone()).map_err(Error::from)
    }

    pub async fn to_config(&self) -> Result<HoprLibConfig, Error> {
        match self.config_mode.clone() {
            // use user provided configuration path
            ConfigFileMode::Manual(path) => config::from_path(path.as_ref()).await.map_err(Error::from),
            // check status of config generation
            ConfigFileMode::Generated => config::generate().await.map_err(Error::from),
        }
    }

    pub fn allow_insecure(&self) -> bool {
        self.allow_insecure
    }

    pub fn blokli_url(&self) -> Option<Url> {
        self.blokli_url.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hopr::config::HoprLibConfig;
    use std::fs;
    use tempfile::NamedTempFile;
    use tokio::runtime::Runtime;

    fn params_with_mode(mode: ConfigFileMode) -> HoprParams {
        HoprParams::new(None, None, mode, true, None)
    }

    fn rt() -> Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt")
    }

    #[test]
    fn manual_mode_reads_hopr_config_from_file() -> anyhow::Result<()> {
        let temp = NamedTempFile::new().expect("temp config");
        let config = HoprLibConfig::default();
        let yaml = serde_yaml::to_string(&config).expect("yaml");

        fs::write(temp.path(), yaml).expect("write config");

        let params = params_with_mode(ConfigFileMode::Manual(temp.path().to_path_buf()));
        let cfg = rt().block_on(params.to_config(Balance::<WxHOPR>::default()))?;

        assert_eq!(cfg, config);
        Ok(())
    }

    #[test]
    fn manual_mode_propagates_parsing_error() -> anyhow::Result<()> {
        let temp = NamedTempFile::new().expect("temp config");
        fs::write(temp.path(), "invalid: [::yaml").expect("write invalid");

        let params = params_with_mode(ConfigFileMode::Manual(temp.path().to_path_buf()));
        let err = rt()
            .block_on(params.to_config(Balance::<WxHOPR>::default()))
            .expect_err("invalid config should bubble up parse error");

        assert!(matches!(err, Error::Config(_)));
        Ok(())
    }
}
