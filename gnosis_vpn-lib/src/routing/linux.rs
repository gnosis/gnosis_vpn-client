use tokio::process::Command;

use crate::worker;

use super::Error;

#[derive(Debug)]
pub struct Routing {
    worker: worker::Worker,
}

/**
 * Refactor logic to use:
 * - [rtnetlink](https://docs.rs/rtnetlink/latest/rtnetlink/index.html)
 */
impl Routing {
    pub fn new(worker: worker::Worker) -> Result<Self, Error> {
        Ok(Routing { worker })
    }

    pub async fn setup(&mut self) -> Result<(), Error> {
        let output = Command::new("ip")
            .arg("rule")
            .arg("add")
            .arg("uidrange")
            .arg(format!("{}-{}", self.worker.uid, self.worker.uid))
            .arg("lookup")
            .arg("main")
            .arg("priority")
            .arg("100")
            .output()
            .await?;

        let stderrempty = output.stderr.is_empty();
        match (stderrempty, output.status.success()) {
            (true, true) => Ok(()),
            (false, true) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(%stderr, "Non empty stderr on successful ip rule addition");
                Ok(())
            }
            (_, false) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let status_code = output.status.code();
                tracing::error!(?status_code, %stdout, %stderr, "Error executing ip rule addition");
                Err(Error::IpRuleSetup)
            }
        }
    }
}
