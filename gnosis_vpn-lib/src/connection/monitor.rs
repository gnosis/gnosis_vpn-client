use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};
use std::time::SystemTime;

use crate::log_output;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Monitor {
    pub since: SystemTime,
}

impl Monitor {
    pub fn new() -> Self {
        Self {
            since: SystemTime::now(),
        }
    }

    pub fn reset_success(self) -> Monitor {
        Monitor { since: self.since }
    }
}

impl Display for Monitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "since {}", log_output::elapsed(&self.since),)
    }
}
