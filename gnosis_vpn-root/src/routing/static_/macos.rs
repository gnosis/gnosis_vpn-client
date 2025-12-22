use crate::routing::Error;
use crate::routing::RoutingTrait;

use super::Static;

impl RoutingTrait for Static {
    async fn setup(&self) -> Result<(), Error> {
        Err(Error::NotImplemented)
    }

    async fn teardown(&self) -> Result<(), Error> {
        Err(Error::NotImplemented)
    }
}
