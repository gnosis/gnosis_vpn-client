//! The service's HOPR-typed values in their protocol form.
use edgli::hopr_lib::HopRouting;
use edgli::hopr_lib::api::types::primitive::prelude::{Address, Balance, WxHOPR, XDai};
use gnosis_vpn_ipc as ipc;

pub fn address(address: &Address) -> ipc::Address {
    ipc::Address::new((*address).into())
}

pub fn wxhopr(balance: Balance<WxHOPR>) -> ipc::Balance<ipc::WxHopr> {
    ipc::Balance::from_wei(wei(&balance))
}

pub fn xdai(balance: Balance<XDai>) -> ipc::Balance<ipc::XDai> {
    ipc::Balance::from_wei(wei(&balance))
}

pub fn hops(routing: HopRouting) -> ipc::Hops {
    let count = u8::try_from(routing.hop_count()).unwrap_or(u8::MAX);
    ipc::Hops::try_from(count).expect("HopRouting caps hops at the protocol's maximum")
}

fn wei<C: edgli::hopr_lib::api::types::primitive::prelude::Currency>(balance: &Balance<C>) -> u128 {
    // Both token supplies are far below u128::MAX wei.
    u128::try_from(balance.amount()).unwrap_or(u128::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hopr_addresses_keep_their_checksum() {
        let address: Address = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed".parse().expect("address");
        assert_eq!(super::address(&address).to_string(), address.to_checksum());
    }

    #[test]
    fn hopr_balances_keep_their_wei() {
        let balance: Balance<WxHOPR> = "1.5 wxHOPR".parse().expect("balance");
        assert_eq!(wxhopr(balance).wei(), 1_500_000_000_000_000_000);
        assert_eq!(wxhopr(balance).to_string(), "1.5 wxHOPR");
    }

    #[test]
    fn every_hop_routing_fits_the_protocol() {
        for count in 0..=HopRouting::MAX_HOPS {
            let routing = HopRouting::try_from(count).expect("valid hop count");
            assert_eq!(usize::from(hops(routing).count()), count);
        }
    }
}
