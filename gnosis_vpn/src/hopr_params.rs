pub struct HoprParams {
    pub identity_file: Option<PathBuf>,
    pub identity_pass: Option<String>,
    pub config_mode: ConfigMode,
}

pub enum ConfigMode {
    Manual(PathBuf),
    Generated { rpc_provider: Url, network: String },
}
