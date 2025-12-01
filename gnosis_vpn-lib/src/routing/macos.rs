use crate::user;

use super::Error;

#[derive(Debug)]
pub struct Routing {
    worker: user::Worker,
}
