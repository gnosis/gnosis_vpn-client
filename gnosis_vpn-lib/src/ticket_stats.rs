use edgli::hopr_lib::api::types::primitive::prelude::{Balance, WxHOPR};
use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};

use crate::serde_utils;

#[derive(Copy, Debug, Clone, Serialize, Deserialize)]
pub struct TicketStats {
    #[serde(with = "serde_utils::balance")]
    pub ticket_price: Balance<WxHOPR>,
    pub winning_probability: f64,
}
