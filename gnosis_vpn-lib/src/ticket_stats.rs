use edgli::hopr_lib::UnitaryFloatOps;
use edgli::hopr_lib::{Balance, GeneralError, WxHOPR};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Error calculating ticket price: {0}")]
    Hopr(#[from] GeneralError),
}

#[derive(Copy, Debug, Clone)]
pub struct TicketStats {
    pub ticket_value: Balance<WxHOPR>,
    pub winning_probability: f64,
}

impl TicketStats {
    pub fn new(ticket_value: Balance<WxHOPR>, winning_probability: f64) -> Self {
        Self {
            ticket_value,
            winning_probability,
        }
    }

    pub fn ticket_price(&self) -> Result<Balance<WxHOPR>, Error> {
        self.ticket_value.div_f64(self.winning_probability).map_err(Error::Hopr)
    }
}
