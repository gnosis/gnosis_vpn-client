use tokio::process::Command;

use crate::util::CommandExt;
use crate::worker;

use super::Error;

/**
 * Refactor logic to use:
 * - [rtnetlink](https://docs.rs/rtnetlink/latest/rtnetlink/index.html)
 */
pub async fn setup(worker: &worker::Worker) -> Result<(), Error> {
    Command::new("ip")
        .arg("rule")
        .arg("add")
        .arg("uidrange")
        .arg(format!("{}-{}", worker.uid, worker.uid))
        .arg("lookup")
        .arg("main")
        .arg("priority")
        .arg("100")
        .run()
        .await
        .map_err(Error::from)
}
