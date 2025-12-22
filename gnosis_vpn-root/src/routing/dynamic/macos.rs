use crate::routing::Error;
use crate::routing::RoutingTrait;

use super::Dynamic;

impl RoutingTrait for Dynamic {
    async fn setup(&self) -> Result<(), Error> {
        Err(Error::NotImplemented)
    }

    async fn teardown(&self) -> Result<(), Error> {
        Err(Error::NotImplemented)
    }
}
