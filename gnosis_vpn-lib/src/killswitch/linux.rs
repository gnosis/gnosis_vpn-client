//! Killswitch firewall for gnosis_vpn.
//!
//! Implements default-drop filtering via an nftables table (`gnosis_vpn_ks`).
//! All traffic is blocked unless it matches an explicit allowlist:
//! - Loopback interface
//! - DHCP (v4 + v6) client traffic
//! - NDP (IPv6 neighbor discovery)
//! - WireGuard tunnel interface — name-based matching (`meta oifname`/`iifname`)
//!   so the rule safely no-ops when `wg0_gnosisvpn` doesn't yet exist
//! - Outbound to each explicitly listed IP; inbound from each IP if ESTABLISHED

use std::ffi::CString;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnetwork::{IpNetwork, Ipv6Network};
use nftnl::{
    Batch, Chain, FinalizedBatch, Hook, MsgType, Policy, ProtoFamily, Rule, Table,
    expr::{self, Payload, Verdict},
    nft_expr,
};
use thiserror::Error;

use crate::wireguard::WG_INTERFACE;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    NfTables(String),
}

const TABLE_NAME: &std::ffi::CStr = c"gnosis_vpn_ks";
const IN_CHAIN_NAME: &std::ffi::CStr = c"input";
const OUT_CHAIN_NAME: &std::ffi::CStr = c"output";
const FORWARD_CHAIN_NAME: &std::ffi::CStr = c"forward";

const DHCPV4_SERVER_PORT: u16 = 67;
const DHCPV4_CLIENT_PORT: u16 = 68;
const DHCPV6_SERVER_PORT: u16 = 547;
const DHCPV6_CLIENT_PORT: u16 = 546;

const IPV6_LINK_LOCAL: Ipv6Network = Ipv6Network::new_checked(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10).unwrap();
const DHCPV6_SERVER_ADDRS: [Ipv6Addr; 2] = [
    Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 1, 2),
    Ipv6Addr::new(0xff05, 0, 0, 0, 0, 0, 1, 3),
];
const ROUTER_SOLICITATION_OUT_DST_ADDR: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 2);
const SOLICITED_NODE_MULTICAST: Ipv6Network =
    Ipv6Network::new_checked(Ipv6Addr::new(0xff02, 0, 0, 0, 0, 1, 0xFF00, 0), 104).unwrap();

#[derive(Clone, Copy)]
enum Direction {
    In,
    Out,
}

#[derive(Clone, Copy)]
enum End {
    Src,
    Dst,
}

#[derive(Clone, Copy)]
enum Protocol {
    Udp,
}

/// Killswitch firewall using nftables.
///
/// Applies a default-drop policy: all traffic blocked unless it matches
/// loopback, DHCP, NDP, the WireGuard tunnel interface, or an explicitly
/// listed IP.
pub struct Firewall;

impl Firewall {
    pub fn new() -> Result<Self, Error> {
        Ok(Firewall)
    }

    /// Apply killswitch policy: block everything except `allowed_ips` and infrastructure.
    pub fn apply_policy(&mut self, allowed_ips: &[IpAddr]) -> Result<(), Error> {
        let table = Table::new(TABLE_NAME, ProtoFamily::Inet);
        let batch = PolicyBatch::new(&table).finalize(allowed_ips);
        send_batch(&batch)
    }

    /// Remove the killswitch table, restoring normal networking.
    pub fn reset_policy(&mut self) -> Result<(), Error> {
        let table = Table::new(TABLE_NAME, ProtoFamily::Inet);
        let mut batch = Batch::new();
        // Add-then-Del avoids ENOENT if the table was never created.
        batch.add(&table, MsgType::Add);
        batch.add(&table, MsgType::Del);
        send_batch(&batch.finalize())
    }
}

struct PolicyBatch<'a> {
    batch: Batch,
    in_chain: Chain<'a>,
    out_chain: Chain<'a>,
    forward_chain: Chain<'a>,
}

impl<'a> PolicyBatch<'a> {
    fn new(table: &'a Table) -> Self {
        let mut batch = Batch::new();

        // Add/Del/Add atomically replaces any existing table on re-apply.
        batch.add(table, MsgType::Add);
        batch.add(table, MsgType::Del);
        batch.add(table, MsgType::Add);

        let mut in_chain = Chain::new(IN_CHAIN_NAME, table);
        in_chain.set_hook(Hook::In, 0);
        in_chain.set_policy(Policy::Drop);
        batch.add(&in_chain, MsgType::Add);

        let mut out_chain = Chain::new(OUT_CHAIN_NAME, table);
        out_chain.set_hook(Hook::Out, 0);
        out_chain.set_policy(Policy::Drop);
        batch.add(&out_chain, MsgType::Add);

        let mut forward_chain = Chain::new(FORWARD_CHAIN_NAME, table);
        forward_chain.set_hook(Hook::Forward, 0);
        forward_chain.set_policy(Policy::Drop);
        batch.add(&forward_chain, MsgType::Add);

        PolicyBatch {
            batch,
            in_chain,
            out_chain,
            forward_chain,
        }
    }

    fn finalize(mut self, allowed_ips: &[IpAddr]) -> FinalizedBatch {
        self.add_loopback_rules();
        self.add_dhcp_client_rules();
        self.add_ndp_rules();
        self.add_tunnel_rules();
        for &ip in allowed_ips {
            self.add_allowed_ip_rules(ip);
        }
        self.batch.finalize()
    }

    fn add_loopback_rules(&mut self) {
        let mut out_rule = Rule::new(&self.out_chain);
        check_iface_by_name(&mut out_rule, Direction::Out, "lo");
        out_rule.add_expr(&Verdict::Accept);
        self.batch.add(&out_rule, MsgType::Add);

        let mut in_rule = Rule::new(&self.in_chain);
        check_iface_by_name(&mut in_rule, Direction::In, "lo");
        in_rule.add_expr(&Verdict::Accept);
        self.batch.add(&in_rule, MsgType::Add);
    }

    fn add_dhcp_client_rules(&mut self) {
        // Outgoing DHCPv4 request: src port 68, dst 255.255.255.255 port 67
        for chain in &[&self.out_chain, &self.forward_chain] {
            let mut rule = Rule::new(chain);
            check_port(&mut rule, Protocol::Udp, End::Src, DHCPV4_CLIENT_PORT);
            check_ip(&mut rule, End::Dst, IpAddr::V4(Ipv4Addr::BROADCAST));
            check_port(&mut rule, Protocol::Udp, End::Dst, DHCPV4_SERVER_PORT);
            rule.add_expr(&Verdict::Accept);
            self.batch.add(&rule, MsgType::Add);
        }
        // Incoming DHCPv4 response: src port 67, dst port 68
        for chain in &[&self.in_chain, &self.forward_chain] {
            let mut rule = Rule::new(chain);
            check_port(&mut rule, Protocol::Udp, End::Src, DHCPV4_SERVER_PORT);
            check_port(&mut rule, Protocol::Udp, End::Dst, DHCPV4_CLIENT_PORT);
            rule.add_expr(&Verdict::Accept);
            self.batch.add(&rule, MsgType::Add);
        }
        // Outgoing DHCPv6 request: src fe80::/10, src port 546, dst ff02::1:2/ff05::1:3, dst port 547
        for chain in &[&self.out_chain, &self.forward_chain] {
            for &server in &DHCPV6_SERVER_ADDRS {
                let mut rule = Rule::new(chain);
                check_net(&mut rule, End::Src, IPV6_LINK_LOCAL);
                check_port(&mut rule, Protocol::Udp, End::Src, DHCPV6_CLIENT_PORT);
                check_ip(&mut rule, End::Dst, IpAddr::V6(server));
                check_port(&mut rule, Protocol::Udp, End::Dst, DHCPV6_SERVER_PORT);
                rule.add_expr(&Verdict::Accept);
                self.batch.add(&rule, MsgType::Add);
            }
        }
        // Incoming DHCPv6 response: src fe80::/10 port 547, dst fe80::/10 port 546
        for chain in &[&self.in_chain, &self.forward_chain] {
            let mut rule = Rule::new(chain);
            check_net(&mut rule, End::Src, IPV6_LINK_LOCAL);
            check_port(&mut rule, Protocol::Udp, End::Src, DHCPV6_SERVER_PORT);
            check_net(&mut rule, End::Dst, IPV6_LINK_LOCAL);
            check_port(&mut rule, Protocol::Udp, End::Dst, DHCPV6_CLIENT_PORT);
            rule.add_expr(&Verdict::Accept);
            self.batch.add(&rule, MsgType::Add);
        }
    }

    fn add_ndp_rules(&mut self) {
        // Outgoing Router solicitation (type 133): dst ff02::2
        for chain in &[&self.out_chain, &self.forward_chain] {
            let mut rule = Rule::new(chain);
            check_ip(&mut rule, End::Dst, IpAddr::V6(ROUTER_SOLICITATION_OUT_DST_ADDR));
            check_icmpv6(&mut rule, 133, 0);
            rule.add_expr(&Verdict::Accept);
            self.batch.add(&rule, MsgType::Add);
        }
        // Incoming Router advertisement (type 134): src fe80::/10
        for chain in &[&self.in_chain, &self.forward_chain] {
            let mut rule = Rule::new(chain);
            check_net(&mut rule, End::Src, IPV6_LINK_LOCAL);
            check_icmpv6(&mut rule, 134, 0);
            rule.add_expr(&Verdict::Accept);
            self.batch.add(&rule, MsgType::Add);
        }
        // Incoming Redirect (type 137): src fe80::/10
        for chain in &[&self.in_chain, &self.forward_chain] {
            let mut rule = Rule::new(chain);
            check_net(&mut rule, End::Src, IPV6_LINK_LOCAL);
            check_icmpv6(&mut rule, 137, 0);
            rule.add_expr(&Verdict::Accept);
            self.batch.add(&rule, MsgType::Add);
        }
        // Outgoing Neighbor solicitation (type 135): dst solicited-node multicast
        for chain in &[&self.out_chain, &self.forward_chain] {
            let mut rule = Rule::new(chain);
            check_net(&mut rule, End::Dst, SOLICITED_NODE_MULTICAST);
            check_icmpv6(&mut rule, 135, 0);
            rule.add_expr(&Verdict::Accept);
            self.batch.add(&rule, MsgType::Add);
        }
        // Outgoing Neighbor solicitation (type 135): dst fe80::/10
        for chain in &[&self.out_chain, &self.forward_chain] {
            let mut rule = Rule::new(chain);
            check_net(&mut rule, End::Dst, IPV6_LINK_LOCAL);
            check_icmpv6(&mut rule, 135, 0);
            rule.add_expr(&Verdict::Accept);
            self.batch.add(&rule, MsgType::Add);
        }
        // Incoming Neighbor solicitation (type 135): src fe80::/10
        for chain in &[&self.in_chain, &self.forward_chain] {
            let mut rule = Rule::new(chain);
            check_net(&mut rule, End::Src, IPV6_LINK_LOCAL);
            check_icmpv6(&mut rule, 135, 0);
            rule.add_expr(&Verdict::Accept);
            self.batch.add(&rule, MsgType::Add);
        }
        // Outgoing Neighbor advertisement (type 136): dst fe80::/10
        for chain in &[&self.out_chain, &self.forward_chain] {
            let mut rule = Rule::new(chain);
            check_net(&mut rule, End::Dst, IPV6_LINK_LOCAL);
            check_icmpv6(&mut rule, 136, 0);
            rule.add_expr(&Verdict::Accept);
            self.batch.add(&rule, MsgType::Add);
        }
        // Incoming Neighbor advertisement (type 136)
        for chain in &[&self.in_chain, &self.forward_chain] {
            let mut rule = Rule::new(chain);
            check_icmpv6(&mut rule, 136, 0);
            rule.add_expr(&Verdict::Accept);
            self.batch.add(&rule, MsgType::Add);
        }
    }

    fn add_tunnel_rules(&mut self) {
        // Allow all traffic through the tunnel (name-based: safe when wg0_gnosisvpn doesn't exist)
        let mut out_rule = Rule::new(&self.out_chain);
        check_iface_by_name(&mut out_rule, Direction::Out, WG_INTERFACE);
        out_rule.add_expr(&Verdict::Accept);
        self.batch.add(&out_rule, MsgType::Add);

        let mut in_rule = Rule::new(&self.in_chain);
        check_iface_by_name(&mut in_rule, Direction::In, WG_INTERFACE);
        in_rule.add_expr(&Verdict::Accept);
        self.batch.add(&in_rule, MsgType::Add);

        // Forward out through tunnel
        let mut fwd_out_rule = Rule::new(&self.forward_chain);
        check_iface_by_name(&mut fwd_out_rule, Direction::Out, WG_INTERFACE);
        fwd_out_rule.add_expr(&Verdict::Accept);
        self.batch.add(&fwd_out_rule, MsgType::Add);

        // Forward in from tunnel only if ESTABLISHED — prevents unsolicited traffic leaking in
        let established_bits = nftnl::expr::ct::States::ESTABLISHED.bits();
        let mut fwd_in_rule = Rule::new(&self.forward_chain);
        check_iface_by_name(&mut fwd_in_rule, Direction::In, WG_INTERFACE);
        fwd_in_rule.add_expr(&nft_expr!(ct state));
        fwd_in_rule.add_expr(&nft_expr!(bitwise mask established_bits, xor 0u32));
        fwd_in_rule.add_expr(&nft_expr!(cmp != 0u32));
        fwd_in_rule.add_expr(&Verdict::Accept);
        self.batch.add(&fwd_in_rule, MsgType::Add);
    }

    fn add_allowed_ip_rules(&mut self, ip: IpAddr) {
        // Allow all outgoing traffic to this IP
        let mut out_rule = Rule::new(&self.out_chain);
        check_ip(&mut out_rule, End::Dst, ip);
        out_rule.add_expr(&Verdict::Accept);
        self.batch.add(&out_rule, MsgType::Add);

        // Allow incoming traffic from this IP only if ESTABLISHED (return traffic only)
        let established_bits = nftnl::expr::ct::States::ESTABLISHED.bits();
        let mut in_rule = Rule::new(&self.in_chain);
        check_ip(&mut in_rule, End::Src, ip);
        in_rule.add_expr(&nft_expr!(ct state));
        in_rule.add_expr(&nft_expr!(bitwise mask established_bits, xor 0u32));
        in_rule.add_expr(&nft_expr!(cmp != 0u32));
        in_rule.add_expr(&Verdict::Accept);
        self.batch.add(&in_rule, MsgType::Add);
    }
}

/// Match interface by name (safe to use for non-existent interfaces — rule simply never matches).
fn check_iface_by_name(rule: &mut Rule<'_>, direction: Direction, iface: &str) {
    let cstr = CString::new(iface).expect("interface name contains null byte");
    rule.add_expr(&match direction {
        Direction::In => nft_expr!(meta iifname),
        Direction::Out => nft_expr!(meta oifname),
    });
    rule.add_expr(&nft_expr!(cmp == expr::InterfaceName::Exact(cstr)));
}

fn check_ip(rule: &mut Rule<'_>, end: End, ip: IpAddr) {
    check_l3proto(rule, ip);
    rule.add_expr(&match (ip, end) {
        (IpAddr::V4(..), End::Src) => nft_expr!(payload ipv4 saddr),
        (IpAddr::V4(..), End::Dst) => nft_expr!(payload ipv4 daddr),
        (IpAddr::V6(..), End::Src) => nft_expr!(payload ipv6 saddr),
        (IpAddr::V6(..), End::Dst) => nft_expr!(payload ipv6 daddr),
    });
    match ip {
        IpAddr::V4(addr) => rule.add_expr(&nft_expr!(cmp == addr)),
        IpAddr::V6(addr) => rule.add_expr(&nft_expr!(cmp == addr)),
    }
}

fn check_net(rule: &mut Rule<'_>, end: End, net: impl Into<IpNetwork>) {
    let net = net.into();
    check_l3proto(rule, net.ip());
    rule.add_expr(&match (net, end) {
        (IpNetwork::V4(_), End::Src) => nft_expr!(payload ipv4 saddr),
        (IpNetwork::V4(_), End::Dst) => nft_expr!(payload ipv4 daddr),
        (IpNetwork::V6(_), End::Src) => nft_expr!(payload ipv6 saddr),
        (IpNetwork::V6(_), End::Dst) => nft_expr!(payload ipv6 daddr),
    });
    // Bitwise-AND the packet address with the subnet mask, then compare against the network address
    match net {
        IpNetwork::V4(_) => rule.add_expr(&nft_expr!(bitwise mask net.mask(), xor 0u32)),
        IpNetwork::V6(_) => rule.add_expr(&nft_expr!(bitwise mask net.mask(), xor &[0u16; 8][..])),
    }
    rule.add_expr(&nft_expr!(cmp == net.ip()));
}

fn check_port(rule: &mut Rule<'_>, protocol: Protocol, end: End, port: u16) {
    check_l4proto(rule, protocol);
    rule.add_expr(&match (protocol, end) {
        (Protocol::Udp, End::Src) => nft_expr!(payload udp sport),
        (Protocol::Udp, End::Dst) => nft_expr!(payload udp dport),
    });
    rule.add_expr(&nft_expr!(cmp == port.to_be()));
}

fn check_icmpv6(rule: &mut Rule<'_>, type_: u8, code: u8) {
    rule.add_expr(&nft_expr!(meta l4proto));
    rule.add_expr(&nft_expr!(cmp == libc::IPPROTO_ICMPV6 as u8));
    rule.add_expr(&Payload::Transport(nftnl::expr::TransportHeaderField::Icmpv6(
        nftnl::expr::Icmpv6HeaderField::Type,
    )));
    rule.add_expr(&nft_expr!(cmp == type_));
    rule.add_expr(&Payload::Transport(nftnl::expr::TransportHeaderField::Icmpv6(
        nftnl::expr::Icmpv6HeaderField::Code,
    )));
    rule.add_expr(&nft_expr!(cmp == code));
}

fn check_l3proto(rule: &mut Rule<'_>, ip: IpAddr) {
    let proto = match ip {
        IpAddr::V4(_) => libc::NFPROTO_IPV4 as u8,
        IpAddr::V6(_) => libc::NFPROTO_IPV6 as u8,
    };
    rule.add_expr(&nft_expr!(meta nfproto));
    rule.add_expr(&nft_expr!(cmp == proto));
}

fn check_l4proto(rule: &mut Rule<'_>, protocol: Protocol) {
    let proto = match protocol {
        Protocol::Udp => libc::IPPROTO_UDP as u8,
    };
    rule.add_expr(&nft_expr!(meta l4proto));
    rule.add_expr(&nft_expr!(cmp == proto));
}

fn send_batch(batch: &FinalizedBatch) -> Result<(), Error> {
    let socket = mnl::Socket::new(mnl::Bus::Netfilter)
        .map_err(|e| Error::NfTables(format!("failed to open netlink socket: {e}")))?;
    let portid = socket.portid();

    socket
        .send_all(batch)
        .map_err(|e| Error::NfTables(format!("failed to send batch: {e}")))?;

    let mut buffer = vec![0; nftnl::nft_nlmsg_maxsize() as usize];
    let mut expected_seqs = batch.sequence_numbers();

    while !expected_seqs.is_empty() {
        let messages = socket
            .recv(&mut buffer[..])
            .map_err(|e| Error::NfTables(format!("failed to receive netlink response: {e}")))?;
        for message in messages {
            let message = message.map_err(|e| Error::NfTables(format!("netlink message error: {e}")))?;
            let expected_seq = expected_seqs
                .next()
                .ok_or_else(|| Error::NfTables("unexpected ACK from netfilter".into()))?;
            mnl::cb_run(message, expected_seq, portid)
                .map_err(|e| Error::NfTables(format!("netlink ACK error: {e}")))?;
        }
    }

    Ok(())
}
