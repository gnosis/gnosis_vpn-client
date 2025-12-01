use thiserror::Error;

use gnosis_vpn_lib::USERNAME;

#[derive(Debug, Error)]
pub enum Error {
    #[error("User not found: {USERNAME}")]
    UserNotFound,
}

#[derive(Debug)]
pub struct Worker {
    pub uid: u32,
    pub gid: u32,
}

impl Worker {
    pub fn from_system() -> Result<Self, Error> {
        users::get_user_by_name(USERNAME)
            .map(|user| {
                let uid = user.uid();
                let gid = user.primary_group_id();
                Worker { uid, gid }
            })
            .ok_or(Error::UserNotFound)
    }
}
