use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;
use gnosis_vpn_lib::worker;

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

pub async fn teardown(worker: &worker::Worker) -> Result<(), Error> {
    Command::new("ip")
        .arg("rule")
        .arg("del")
        .arg("uidrange")
        .arg(format!("{}-{}", worker.uid, worker.uid))
        .arg("lookup")
        .arg("main")
        .arg("priority")
        .arg("100")
        .spawn_no_capture()
        .await
        .map_err(Error::from)
}
