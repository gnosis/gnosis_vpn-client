use super::Error;

#[derive(Debug)]
pub struct Routing {
    worker: user::Worker,
}

/**
 * Refactor logic to use:
 * - [rtnetlink](https://docs.rs/rtnetlink/latest/rtnetlink/index.html)
 */
impl Routing {
    pub fn new(worker: user::Worker) -> Result<Self, Error> {
        Ok(Routing { worker })
    }

    pub fn setup(&mut self) -> Result<(), Error> {
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

        if output.status.success() {
            Ok(())
        } else {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let status_code = output.status.code();
            tracing::error!(%status_code, %stdout, %stderr, "Failed to add ip rule");
            Err(Error::IpRuleSetup(status_code))
        }
    }
}
