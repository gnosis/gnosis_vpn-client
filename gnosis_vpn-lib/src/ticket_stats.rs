use edgli::hopr_lib::UnitaryFloatOps;
use edgli::hopr_lib::{Balance, GeneralError, WxHOPR};
use thiserror::Error;

use std::fmt::{self, Display};

#[derive(Debug, Error)]
pub enum Error {
    #[error("Error calculating ticket price: {0}")]
    Hopr(#[from] GeneralError),
}

#[derive(Copy, Debug, Clone)]
pub struct TicketStats {
    pub ticket_price: Balance<WxHOPR>,
    pub winning_probability: f64,
}

impl TicketStats {
    pub fn new(ticket_price: Balance<WxHOPR>, winning_probability: f64) -> Self {
        Self {
            ticket_price,
            winning_probability,
        }
    }

    /// Calculate ticket value from onchain ticket price and winning probability
    pub fn ticket_value(&self) -> Result<Balance<WxHOPR>, Error> {
        self.ticket_price.div_f64(self.winning_probability).map_err(Error::Hopr)
    }
}

impl Display for TicketStats {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Ticket value: {:?}, Winning probability: {:.4}",
            self.ticket_price, self.winning_probability,
        )
    }
}
