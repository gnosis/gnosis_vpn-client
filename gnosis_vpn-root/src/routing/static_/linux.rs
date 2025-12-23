use async_trait::async_trait;

use crate::routing::Error;
use crate::routing::Routing;

use super::Static;

#[async_trait]
impl Routing for Static {
    async fn setup(&self) -> Result<(), Error> {
        Err(Error::NotImplemented)
    }

    async fn teardown(&self) -> Result<(), Error> {
        Err(Error::NotImplemented)
    }
}
