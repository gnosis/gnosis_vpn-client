use async_trait::async_trait;

use crate::routing::Error;
use crate::routing::{Routing, util};
use crate::wg_tooling;

use super::Static;

#[async_trait]
impl Routing for Static {
    async fn setup(&self) -> Result<(), Error> {
        let interface_gateway = util::interface().await?;
        let mut extra = self
            .peer_ips
            .iter()
            .map(|ip| util::pre_up_routing(ip, interface_gateway.clone()))
            .collect::<Vec<String>>();
        extra.extend(
            self.peer_ips
                .iter()
                .map(|ip| util::post_down_routing(ip, interface_gateway.clone()))
                .collect::<Vec<String>>(),
        );

        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, true, Some(extra));
        wg_tooling::up(wg_quick_content).await?;
        Ok(())
    }

    async fn teardown(&self) -> Result<(), Error> {
        wg_tooling::down().await?;
        Ok(())
    }
}
