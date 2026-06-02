use edgli::hopr_lib::api::types::internal::protocol::HoprPseudonym;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::connection::destination::{Address, Destination, HopRouting};

/// Caches session pseudonyms keyed by (destination address, routing).
///
/// Keying on routing as well as address ensures that reconnecting to the same
/// exit node via a different hop count does not reuse a pseudonym that was
/// issued for a different path, which would cause the exit node to reject it.
pub struct PseudonymCache {
    inner: HashMap<(Address, HopRouting), (HoprPseudonym, Instant)>,
    ttl: Duration,
}

impl PseudonymCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: HashMap::new(),
            ttl,
        }
    }

    /// Returns a cached pseudonym for `dest` if one exists within the TTL.
    ///
    /// Expired entries are evicted on each call to prevent stale pseudonyms
    /// accumulating as destinations or routing options change over time.
    pub fn get(&mut self, dest: &Destination) -> Option<HoprPseudonym> {
        let ttl = self.ttl;
        self.inner.retain(|_, (_, cached_at)| cached_at.elapsed() < ttl);
        self.inner
            .get(&(dest.address, dest.routing))
            .map(|(pseudonym, _)| *pseudonym)
    }

    pub fn insert(&mut self, dest: &Destination, pseudonym: HoprPseudonym) {
        self.inner
            .insert((dest.address, dest.routing), (pseudonym, Instant::now()));
    }

    pub fn remove(&mut self, dest: &Destination) {
        self.inner.remove(&(dest.address, dest.routing));
    }
}
