use crate::balance::{Balance, WxHOPR};

use super::HoprError;

/// The Curvy pool's shielded float that PIX deposits are paid from.
#[derive(Clone)]
pub struct PixFloat(
    #[cfg(feature = "pix-curvy")] edgli::PixFloatHandle,
    // `pix-test` pays straight from the Safe, so it never has a float to construct.
    #[cfg(not(feature = "pix-curvy"))] std::convert::Infallible,
);

impl PixFloat {
    pub fn of(reactor: &edgli::ReactorHandle) -> Option<Self> {
        #[cfg(feature = "pix-curvy")]
        return reactor.pix_float.clone().map(Self);
        #[cfg(not(feature = "pix-curvy"))]
        {
            let _ = reactor;
            None
        }
    }

    pub async fn balance(&self) -> Result<Balance<WxHOPR>, HoprError> {
        #[cfg(feature = "pix-curvy")]
        return self
            .0
            .available_balance()
            .await
            .map_err(|e| HoprError::PixFloat(e.to_string()));
        #[cfg(not(feature = "pix-curvy"))]
        match self.0 {}
    }

    pub async fn fund(&self, amount: Balance<WxHOPR>) -> Result<(), HoprError> {
        #[cfg(feature = "pix-curvy")]
        return self
            .0
            .fund(amount)
            .await
            .map_err(|e| HoprError::PixFloat(e.to_string()));
        #[cfg(not(feature = "pix-curvy"))]
        {
            let _ = amount;
            match self.0 {}
        }
    }
}
