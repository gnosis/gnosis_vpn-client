use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnetwork::{IpNetwork, Ipv6Network};
use pfctl::{DropAction, FilterRuleAction};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    PacketFilter(#[from] pfctl::Error),
}

const ANCHOR_NAME: &str = "gnosis_vpn_ks";

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

/// Killswitch firewall using macOS PF (packet filter).
///
/// Applies a default-drop policy via a named PF anchor (`gnosis_vpn_ks`).
/// All traffic is blocked unless it matches loopback, DHCP, NDP, the
/// WireGuard tunnel interface, or an explicitly listed IP.
pub struct Firewall {
    pf: pfctl::PfCtl,
    // Saved so reset_policy can restore PF to its original state.
    pf_was_enabled: Option<bool>,
}

impl Firewall {
    pub fn new() -> Result<Self, Error> {
        Ok(Firewall {
            pf: pfctl::PfCtl::new()?,
            pf_was_enabled: None,
        })
    }

    /// Apply killswitch policy: block everything except `allowed_ips` and infrastructure.
    /// `interface` is the resolved WireGuard interface name (e.g. "utun8" on macOS).
    pub fn apply_policy(&mut self, interface: &str, allowed_ips: &[IpAddr]) -> Result<(), Error> {
        self.enable()?;
        self.add_anchor()?;
        self.set_rules(interface, allowed_ips)?;
        self.flush_states();
        Ok(())
    }

    /// Remove the killswitch anchor, restoring normal networking.
    pub fn reset_policy(&mut self) -> Result<(), Error> {
        // Run all three even on partial failure; return first error encountered.
        let rules_result = self.remove_rules();
        let anchor_result = self.remove_anchor();
        let state_result = self.restore_state();
        rules_result.and(anchor_result).and(state_result)
    }

    fn enable(&mut self) -> Result<(), Error> {
        if self.pf_was_enabled.is_none() {
            self.pf_was_enabled = Some(self.pf.is_enabled().unwrap_or(false));
        }
        Ok(self.pf.try_enable()?)
    }

    fn add_anchor(&mut self) -> Result<(), Error> {
        self.pf.try_add_anchor(ANCHOR_NAME, pfctl::AnchorKind::Scrub)?;
        self.pf.try_add_anchor(ANCHOR_NAME, pfctl::AnchorKind::Filter)?;
        Ok(())
    }

    fn set_rules(&mut self, interface: &str, allowed_ips: &[IpAddr]) -> Result<(), Error> {
        let mut rules = vec![];

        rules.append(&mut loopback_rules()?);
        rules.append(&mut dhcp_rules()?);
        rules.append(&mut ndp_rules()?);
        rules.push(tunnel_rule(interface)?);
        for &ip in allowed_ips {
            rules.append(&mut allowed_ip_rules(ip)?);
        }
        rules.append(&mut drop_rules()?);

        let mut anchor_change = pfctl::AnchorChange::new();
        anchor_change.set_scrub_rules(scrub_rules()?);
        anchor_change.set_filter_rules(rules);
        Ok(self.pf.set_rules(ANCHOR_NAME, anchor_change)?)
    }

    fn remove_rules(&mut self) -> Result<(), Error> {
        self.pf.flush_rules(ANCHOR_NAME, pfctl::RulesetKind::Filter)?;
        self.pf.flush_rules(ANCHOR_NAME, pfctl::RulesetKind::Scrub)?;
        Ok(())
    }

    fn remove_anchor(&mut self) -> Result<(), Error> {
        self.pf.try_remove_anchor(ANCHOR_NAME, pfctl::AnchorKind::Scrub)?;
        self.pf.try_remove_anchor(ANCHOR_NAME, pfctl::AnchorKind::Filter)?;
        Ok(())
    }

    fn restore_state(&mut self) -> Result<(), Error> {
        match self.pf_was_enabled.take() {
            Some(true) => Ok(self.pf.try_enable()?),
            Some(false) => Ok(self.pf.try_disable()?),
            None => Ok(()),
        }
    }

    /// Kill all non-loopback PF states so the new rules take effect on existing connections.
    fn flush_states(&mut self) {
        let states = match self.pf.get_states() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(?e, "failed to get PF states for flushing");
                return;
            }
        };
        for state in states {
            let is_loopback = state.local_address().map(|a| a.ip().is_loopback()).unwrap_or(true); // keep state if we can't parse it
            if !is_loopback {
                if let Err(e) = self.pf.kill_state(&state) {
                    tracing::warn!(?e, "failed to kill PF state");
                }
            }
        }
    }
}

fn scrub_rules() -> Result<Vec<pfctl::ScrubRule>, Error> {
    let rule = pfctl::ScrubRuleBuilder::default()
        .action(pfctl::ScrubRuleAction::Scrub)
        .build()?;
    Ok(vec![rule])
}

fn loopback_rules() -> Result<Vec<pfctl::FilterRule>, Error> {
    let rule = pfctl::FilterRuleBuilder::default()
        .action(FilterRuleAction::Pass)
        .quick(true)
        .interface("lo0")
        .keep_state(pfctl::StatePolicy::Keep)
        .build()?;
    Ok(vec![rule])
}

fn dhcp_rules() -> Result<Vec<pfctl::FilterRule>, Error> {
    let mut rules = vec![];

    // DHCPv4 out: src port 68 → 255.255.255.255:67
    rules.push(
        pfctl::FilterRuleBuilder::default()
            .action(FilterRuleAction::Pass)
            .quick(true)
            .af(pfctl::AddrFamily::Ipv4)
            .proto(pfctl::Proto::Udp)
            .direction(pfctl::Direction::Out)
            .from(pfctl::Port::from(DHCPV4_CLIENT_PORT))
            .to(pfctl::Endpoint::new(
                Ipv4Addr::BROADCAST,
                pfctl::Port::from(DHCPV4_SERVER_PORT),
            ))
            .build()?,
    );
    // DHCPv4 in: src port 67 → dst port 68
    rules.push(
        pfctl::FilterRuleBuilder::default()
            .action(FilterRuleAction::Pass)
            .quick(true)
            .af(pfctl::AddrFamily::Ipv4)
            .proto(pfctl::Proto::Udp)
            .direction(pfctl::Direction::In)
            .from(pfctl::Port::from(DHCPV4_SERVER_PORT))
            .to(pfctl::Port::from(DHCPV4_CLIENT_PORT))
            .build()?,
    );

    // DHCPv6 out: fe80::/10 port 546 → server port 547
    for &server in &DHCPV6_SERVER_ADDRS {
        rules.push(
            pfctl::FilterRuleBuilder::default()
                .action(FilterRuleAction::Pass)
                .quick(true)
                .af(pfctl::AddrFamily::Ipv6)
                .proto(pfctl::Proto::Udp)
                .direction(pfctl::Direction::Out)
                .from(pfctl::Endpoint::new(
                    IpNetwork::V6(IPV6_LINK_LOCAL),
                    pfctl::Port::from(DHCPV6_CLIENT_PORT),
                ))
                .to(pfctl::Endpoint::new(server, pfctl::Port::from(DHCPV6_SERVER_PORT)))
                .build()?,
        );
    }
    // DHCPv6 in: fe80::/10 port 547 → fe80::/10 port 546
    rules.push(
        pfctl::FilterRuleBuilder::default()
            .action(FilterRuleAction::Pass)
            .quick(true)
            .af(pfctl::AddrFamily::Ipv6)
            .proto(pfctl::Proto::Udp)
            .direction(pfctl::Direction::In)
            .from(pfctl::Endpoint::new(
                pfctl::Ip::from(IpNetwork::V6(IPV6_LINK_LOCAL)),
                pfctl::Port::from(DHCPV6_SERVER_PORT),
            ))
            .to(pfctl::Endpoint::new(
                pfctl::Ip::from(IpNetwork::V6(IPV6_LINK_LOCAL)),
                pfctl::Port::from(DHCPV6_CLIENT_PORT),
            ))
            .build()?,
    );

    Ok(rules)
}

fn ndp_rules() -> Result<Vec<pfctl::FilterRule>, Error> {
    use pfctl::{Icmp6Type, IcmpType};

    let base = || {
        let mut b = pfctl::FilterRuleBuilder::default();
        b.action(FilterRuleAction::Pass)
            .quick(true)
            .af(pfctl::AddrFamily::Ipv6)
            .proto(pfctl::Proto::IcmpV6);
        b
    };

    Ok(vec![
        // Router solicitation (133) out → ff02::2
        base()
            .direction(pfctl::Direction::Out)
            .icmp_type(IcmpType::Icmp6(Icmp6Type::RouterSol))
            .to(ROUTER_SOLICITATION_OUT_DST_ADDR)
            .build()?,
        // Router advertisement (134) in ← fe80::/10
        base()
            .direction(pfctl::Direction::In)
            .icmp_type(IcmpType::Icmp6(Icmp6Type::RouterAdv))
            .from(pfctl::Ip::from(IpNetwork::V6(IPV6_LINK_LOCAL)))
            .build()?,
        // Redirect (137) in ← fe80::/10
        base()
            .direction(pfctl::Direction::In)
            .icmp_type(IcmpType::Icmp6(Icmp6Type::Redir))
            .from(pfctl::Ip::from(IpNetwork::V6(IPV6_LINK_LOCAL)))
            .build()?,
        // Neighbor solicitation (135) out → solicited-node multicast
        base()
            .direction(pfctl::Direction::Out)
            .icmp_type(IcmpType::Icmp6(Icmp6Type::NeighbrSol))
            .to(pfctl::Ip::from(IpNetwork::V6(SOLICITED_NODE_MULTICAST)))
            .build()?,
        // Neighbor solicitation (135) out → fe80::/10
        base()
            .direction(pfctl::Direction::Out)
            .icmp_type(IcmpType::Icmp6(Icmp6Type::NeighbrSol))
            .to(pfctl::Ip::from(IpNetwork::V6(IPV6_LINK_LOCAL)))
            .build()?,
        // Neighbor solicitation (135) in ← fe80::/10
        base()
            .direction(pfctl::Direction::In)
            .icmp_type(IcmpType::Icmp6(Icmp6Type::NeighbrSol))
            .from(pfctl::Ip::from(IpNetwork::V6(IPV6_LINK_LOCAL)))
            .build()?,
        // Neighbor advertisement (136) out → fe80::/10
        base()
            .direction(pfctl::Direction::Out)
            .icmp_type(IcmpType::Icmp6(Icmp6Type::NeighbrAdv))
            .to(pfctl::Ip::from(IpNetwork::V6(IPV6_LINK_LOCAL)))
            .build()?,
        // Neighbor advertisement (136) in ← anywhere
        base()
            .direction(pfctl::Direction::In)
            .icmp_type(IcmpType::Icmp6(Icmp6Type::NeighbrAdv))
            .build()?,
    ])
}

fn tunnel_rule(interface: &str) -> Result<pfctl::FilterRule, Error> {
    Ok(pfctl::FilterRuleBuilder::default()
        .action(FilterRuleAction::Pass)
        .quick(true)
        .interface(interface)
        .keep_state(pfctl::StatePolicy::Keep)
        .build()?)
}

fn allowed_ip_rules(ip: IpAddr) -> Result<Vec<pfctl::FilterRule>, Error> {
    // pfctl::Ip::from takes IpNetwork; IpNetwork::from(IpAddr) creates a host /32 or /128.
    let out_rule = pfctl::FilterRuleBuilder::default()
        .action(FilterRuleAction::Pass)
        .quick(true)
        .direction(pfctl::Direction::Out)
        .to(pfctl::Ip::from(IpNetwork::from(ip)))
        .keep_state(pfctl::StatePolicy::Keep)
        .build()?;
    let in_rule = pfctl::FilterRuleBuilder::default()
        .action(FilterRuleAction::Pass)
        .quick(true)
        .direction(pfctl::Direction::In)
        .from(pfctl::Ip::from(IpNetwork::from(ip)))
        .keep_state(pfctl::StatePolicy::Keep)
        .build()?;
    Ok(vec![out_rule, in_rule])
}

fn drop_rules() -> Result<Vec<pfctl::FilterRule>, Error> {
    // Send TCP RST / ICMP unreachable for outbound traffic (less disruptive than silent drop).
    let return_out = pfctl::FilterRuleBuilder::default()
        .action(FilterRuleAction::Drop(DropAction::Return))
        .quick(true)
        .direction(pfctl::Direction::Out)
        .build()?;
    // Silently drop everything else.
    let drop_all = pfctl::FilterRuleBuilder::default()
        .action(FilterRuleAction::Drop(DropAction::Drop))
        .quick(true)
        .build()?;
    Ok(vec![return_out, drop_all])
}
