// do something like addressable peers which has ipv4 guaranteed
pub struct Peer {
    addr: Address,
    ipv4: Option<Ipv4Addr>,
}
