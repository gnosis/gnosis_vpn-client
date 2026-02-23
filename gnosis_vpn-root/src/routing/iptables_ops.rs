//! Abstraction over iptables operations for testability.
//!
//! Defines [`IptablesOps`] trait that mirrors the `iptables` crate API.
//! Production code uses [`RealIptablesOps`].
//! Tests use stateful mocks (see `mocks` module).

/// Abstraction over iptables chain and rule operations.
///
/// All methods are synchronous, matching the underlying `iptables` crate.
pub trait IptablesOps: Send + Sync {
    fn chain_exists(&self, table: &str, chain: &str) -> Result<bool, Box<dyn std::error::Error>>;
    fn new_chain(&self, table: &str, chain: &str) -> Result<(), Box<dyn std::error::Error>>;
    fn flush_chain(&self, table: &str, chain: &str) -> Result<(), Box<dyn std::error::Error>>;
    fn delete_chain(&self, table: &str, chain: &str) -> Result<(), Box<dyn std::error::Error>>;
    fn append(&self, table: &str, chain: &str, rule: &str)
        -> Result<(), Box<dyn std::error::Error>>;
    fn delete(&self, table: &str, chain: &str, rule: &str)
        -> Result<(), Box<dyn std::error::Error>>;
    fn exists(&self, table: &str, chain: &str, rule: &str)
        -> Result<bool, Box<dyn std::error::Error>>;
    fn list(&self, table: &str, chain: &str)
        -> Result<Vec<String>, Box<dyn std::error::Error>>;
}

/// Production [`IptablesOps`] backed by the `iptables` crate.
pub struct RealIptablesOps {
    inner: iptables::IPTables,
}

impl RealIptablesOps {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            inner: iptables::new(false)?,
        })
    }
}

impl IptablesOps for RealIptablesOps {
    fn chain_exists(&self, table: &str, chain: &str) -> Result<bool, Box<dyn std::error::Error>> {
        self.inner.chain_exists(table, chain)
    }

    fn new_chain(&self, table: &str, chain: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.new_chain(table, chain)
    }

    fn flush_chain(&self, table: &str, chain: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.flush_chain(table, chain)
    }

    fn delete_chain(&self, table: &str, chain: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.delete_chain(table, chain)
    }

    fn append(
        &self,
        table: &str,
        chain: &str,
        rule: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.append(table, chain, rule)
    }

    fn delete(
        &self,
        table: &str,
        chain: &str,
        rule: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.delete(table, chain, rule)
    }

    fn exists(
        &self,
        table: &str,
        chain: &str,
        rule: &str,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        self.inner.exists(table, chain, rule)
    }

    fn list(
        &self,
        table: &str,
        chain: &str,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        self.inner.list(table, chain)
    }
}
