use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};
use std::time::SystemTime;

use crate::log_output;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Monitor {
    pub since: SystemTime,
    pub success_count: u16,
}

impl Monitor {
    pub fn new() -> Self {
        Self {
            since: SystemTime::now(),
            success_count: 0,
        }
    }

    pub fn add_success(self) -> Monitor {
        Monitor {
            since: self.since,
            success_count: self.success_count + 1,
        }
    }

    pub fn reset_success(self) -> Monitor {
        Monitor {
            since: self.since,
            success_count: 0,
        }
    }
}

impl Display for Monitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "since {}, {} continuous successes",
            log_output::elapsed(&self.since),
            self.success_count
        )
    }
}
