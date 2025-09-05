#[derive(Debug, Serialize, Deserialize)]
pub struct Info {
    pub node_address: Address,
    pub safe_address: Address,
    pub network_health: Health,
}

/// Network health represented with colors, where green is the best and red
/// is the worst possible observed nework quality.
#[derive(Debug, Serialize, Deserialize)]
pub enum Health {
    /// Unknown health, on application startup
    Unknown,
    /// No connection, default
    Red,
    /// Low quality connection to at least 1 public relay
    Orange,
    /// High quality connection to at least 1 public relay
    Yellow,
    /// High quality connection to at least 1 public relay and 1 NAT node
    Green,
}
