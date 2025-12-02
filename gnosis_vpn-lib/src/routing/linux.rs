use tokio::process::Command;

use crate::util::CommandExt;
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
        Command::new("ip")
            .arg("rule")
            .arg("add")
            .arg("uidrange")
            .arg(format!("{}-{}", self.worker.uid, self.worker.uid))
            .arg("lookup")
            .arg("main")
            .arg("priority")
            .arg("100")
            .run()
            .await
            .map_err(Error::from)
    }
}
