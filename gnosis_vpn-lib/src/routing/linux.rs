use crate::user;

use super::Error;

#[derive(Debug)]
pub struct Routing {
    worker: user::Worker,
    strategy: Strategy,
}

#[derive(Debug)]
enum Strategy {
    CgroupBypass,
    IpRule,
}

const CGROUP: &str = "hoprnet";

/**
 * Refactor logic to use:
 * - [rtnetlink](https://docs.rs/rtnetlink/latest/rtnetlink/index.html)
 * - [cgroup_rs](https://docs.rs/cgroups-rs/0.5.0/cgroups_rs/index.html)
 */
impl Routing {
    pub fn new(worker: user::Worker) -> Result<Self, Error> {
        let strategy = if supports_cgroup_bypass()? {
            Strategy::CgroupBypass
        } else {
            Strategy::IpRule
        };

        Ok(Routing { worker, strategy })
    }

    fn supports_cgroup_bypass() -> Result<bool, Error> {
        let output = Command::new("mount").output().await?;

        if output.status.success() {
Ok(output.contains("cgroup2 "))
        } else {
           let stdout =  String::from_utf8_lossy(&output.stdout);
           let stderr =  String::from_utf8_lossy(&output.stderr);
               tracing::error!(%stdout, %stderr, status = %output.status, "cgroup check mount command failed");
               Err(Error::MountCmdFailed)
        }
    }

    pub fn setup(&mut self) -> Result<(), Error> {
        let output = Command::new("mkdir").arg("-p").arg(format!("/sys/fs/cgroup/net_cls/{CGROUP}")).
            status().await?
echo 0xCAFE > /sys/fs/cgroup/net_cls/bypass_vpn/net_cls.classid
    }
}
