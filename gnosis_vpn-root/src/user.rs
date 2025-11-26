use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("User {NAME} not found")]
    UserNotFound,
}

pub struct Worker {
    pub uid: u32,
    pub gid: u32,
}

const NAME: &str = "gnosisvpn";

impl Worker {
    pub fn from_system() -> Result<Self, Error> {
        users::get_user_by_name(NAME)
            .map(|user| {
                let uid = user.uid();
                let gid = user.primary_group_id();
                Worker { uid, gid }
            })
            .ok_or(Error::UserNotFound)
    }
}
