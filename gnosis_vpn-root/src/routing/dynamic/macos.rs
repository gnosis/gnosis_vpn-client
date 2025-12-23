use async_trait::async_trait;

use crate::routing::Error;
use crate::routing::Routing;

use super::Dynamic;

#[async_trait]
impl Routing for Dynamic {
    async fn setup(&self) -> Result<(), Error> {
        tracing::debug!(wg_data = ?self.wg_data, worker= ?self.worker, "Setting up dynamic routing for macos not yet implemented");
        Err(Error::NotImplemented)
    }

    async fn teardown(&self) -> Result<(), Error> {
        Err(Error::NotImplemented)
    }
}
