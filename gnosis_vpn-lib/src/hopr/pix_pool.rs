use crate::balance::{Balance, WxHOPR};

use super::HoprError;

/// The Curvy PIX pool that PIX deposits are paid from.
#[derive(Clone)]
pub struct PixPool(
    #[cfg(feature = "pix-curvy")] edgli::PixFloatHandle,
    #[cfg(feature = "pix-curvy")] edgli::PixDepositHold,
    // `pix-test` pays straight from the Safe, so it never has a pool to construct.
    #[cfg(not(feature = "pix-curvy"))] std::convert::Infallible,
);

impl PixPool {
    pub fn of(reactor: &edgli::ReactorHandle) -> Option<Self> {
        #[cfg(feature = "pix-curvy")]
        return reactor
            .pix_float
            .clone()
            .zip(reactor.pix_deposits.clone())
            .map(|(float, hold)| Self(float, hold));
        #[cfg(not(feature = "pix-curvy"))]
        {
            let _ = reactor;
            None
        }
    }

    /// Deposits made while held reach the relayer only after the hold is lifted.
    pub fn hold_deposits(&self, held: bool) {
        #[cfg(feature = "pix-curvy")]
        if held {
            self.1.hold();
        } else {
            self.1.release();
        }
        #[cfg(not(feature = "pix-curvy"))]
        {
            let _ = held;
            match self.0 {}
        }
    }

    /// Fails the held deposits without paying them: the only session that pays, the main one, has closed.
    pub fn drop_held_deposits(&self) {
        #[cfg(feature = "pix-curvy")]
        self.1.drop_held_deposits();
        #[cfg(not(feature = "pix-curvy"))]
        match self.0 {}
    }

    pub async fn balance(&self) -> Result<Balance<WxHOPR>, HoprError> {
        #[cfg(feature = "pix-curvy")]
        return self
            .0
            .available_balance()
            .await
            .map_err(|e| HoprError::PixPool(e.to_string()));
        #[cfg(not(feature = "pix-curvy"))]
        match self.0 {}
    }

    pub async fn fund(&self, amount: Balance<WxHOPR>) -> Result<(), HoprError> {
        #[cfg(feature = "pix-curvy")]
        return self.0.fund(amount).await.map_err(|e| HoprError::PixPool(e.to_string()));
        #[cfg(not(feature = "pix-curvy"))]
        {
            let _ = amount;
            match self.0 {}
        }
    }
}
