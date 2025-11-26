use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("User not found: {0}")]
    UserNotFound(String),
}

#[derive(Debug)]
pub struct Worker {
    pub uid: u32,
    pub gid: u32,
}

impl Worker {
    pub fn from_system(name: &str) -> Result<Self, Error> {
        users::get_user_by_name(name)
            .map(|user| {
                let uid = user.uid();
                let gid = user.primary_group_id();
                Worker { uid, gid }
            })
            .ok_or(Error::UserNotFound(name.to_string()))
    }
}
