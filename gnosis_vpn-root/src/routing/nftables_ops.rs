//! Firewall rule management abstraction for fwmark-based routing bypass.
//!
//! Defines [`NfTablesOps`] trait with a higher-level API than the old `IptablesOps`,
//! focusing on the logical operations (setup fwmark rules, teardown, check existence)
//! rather than individual chain/rule manipulation.
//!
//! The current production implementation uses `iptables` under the hood.
//! A future iteration will use native nftables via netlink for CLI-free operation.
//!
//! Production code uses [`RealNfTablesOps`].
//! Tests use stateful mocks (see `mocks` module).

use std::net::Ipv4Addr;

use super::Error;

/// Firewall mark used to identify HOPR traffic for bypass routing.
pub(crate) const FW_MARK: u32 = 0xFEED_CAFE;

/// Custom chain name for HOPR traffic marking.
pub(crate) const CUSTOM_CHAIN: &str = "GNOSIS_VPN";

/// iptables table for packet modification.
const IP_TABLE: &str = "mangle";
/// iptables chain for locally-generated outbound packets.
const IP_CHAIN: &str = "OUTPUT";
/// iptables table for NAT rules.
const NAT_TABLE: &str = "nat";
/// iptables chain for source address translation.
const NAT_CHAIN: &str = "POSTROUTING";

/// Higher-level abstraction over firewall rule management for fwmark bypass.
///
/// Compared to the low-level `IptablesOps` it replaces, this trait operates
/// at the logical level: "set up all fwmark rules" / "tear down everything"
/// rather than individual chain and rule manipulation.
pub trait NfTablesOps: Send + Sync {
    /// Set up all firewall rules needed for fwmark-based routing bypass.
    ///
    /// Creates:
    /// - Mangle rules to mark traffic from `vpn_uid` with `fw_mark`
    /// - NAT SNAT rule to rewrite source address for marked traffic
    fn setup_fwmark_rules(
        &self,
        vpn_uid: u32,
        wan_if_name: &str,
        fw_mark: u32,
        snat_ip: Ipv4Addr,
    ) -> Result<(), Error>;

    /// Tear down all firewall rules for fwmark bypass.
    fn teardown_rules(
        &self,
        wan_if_name: &str,
        fw_mark: u32,
        snat_ip: Ipv4Addr,
    ) -> Result<(), Error>;

    /// Clean up stale fwmark rules from a previous crash.
    fn cleanup_stale_rules(&self, fw_mark: u32) -> Result<(), Error>;
}

/// Production [`NfTablesOps`] backed by the `iptables` crate.
///
/// Uses iptables as the backend. A future migration to native nftables
/// via netlink would only require replacing this implementation while
/// keeping the same trait interface.
pub struct RealNfTablesOps {
    inner: iptables::IPTables,
}

impl RealNfTablesOps {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            inner: iptables::new(false).map_err(Error::iptables)?,
        })
    }
}

impl NfTablesOps for RealNfTablesOps {
    fn setup_fwmark_rules(
        &self,
        vpn_uid: u32,
        wan_if_name: &str,
        fw_mark: u32,
        snat_ip: Ipv4Addr,
    ) -> Result<(), Error> {
        let ipt = &self.inner;

        // Step 1: Create or flush custom chain (idempotent for crash recovery)
        if ipt
            .chain_exists(IP_TABLE, CUSTOM_CHAIN)
            .map_err(Error::iptables)?
        {
            ipt.flush_chain(IP_TABLE, CUSTOM_CHAIN)
                .map_err(Error::iptables)?;
        } else {
            ipt.new_chain(IP_TABLE, CUSTOM_CHAIN)
                .map_err(Error::iptables)?;
        }

        // Step 2: Ensure a single jump rule from OUTPUT -> GNOSIS_VPN
        let jump_rule = format!("-j {CUSTOM_CHAIN}");
        if ipt
            .exists(IP_TABLE, IP_CHAIN, &jump_rule)
            .map_err(Error::iptables)?
        {
            ipt.delete(IP_TABLE, IP_CHAIN, &jump_rule)
                .map_err(Error::iptables)?;
        }
        ipt.append(IP_TABLE, IP_CHAIN, &jump_rule)
            .map_err(Error::iptables)?;

        // Step 3: Keep loopback traffic unmarked
        ipt.append(IP_TABLE, CUSTOM_CHAIN, "-o lo -j RETURN")
            .map_err(Error::iptables)?;

        // Step 4: Mark ALL traffic from VPN user
        ipt.append(
            IP_TABLE,
            CUSTOM_CHAIN,
            &format!("-m owner --uid-owner {vpn_uid} -j MARK --set-mark {fw_mark}"),
        )
        .map_err(Error::iptables)?;

        // Step 5: SNAT for bypassed traffic
        let nat_rule =
            format!("-m mark --mark {fw_mark} -o {wan_if_name} -j SNAT --to-source {snat_ip}");
        if ipt
            .exists(NAT_TABLE, NAT_CHAIN, &nat_rule)
            .map_err(Error::iptables)?
        {
            ipt.delete(NAT_TABLE, NAT_CHAIN, &nat_rule)
                .map_err(Error::iptables)?;
        }
        ipt.append(NAT_TABLE, NAT_CHAIN, &nat_rule)
            .map_err(Error::iptables)?;

        Ok(())
    }

    fn teardown_rules(
        &self,
        wan_if_name: &str,
        fw_mark: u32,
        snat_ip: Ipv4Addr,
    ) -> Result<(), Error> {
        let ipt = &self.inner;
        let mut errors: Vec<String> = Vec::new();

        // Flush chain -> delete jump from OUTPUT -> delete chain
        if ipt
            .chain_exists(IP_TABLE, CUSTOM_CHAIN)
            .unwrap_or(false)
        {
            if let Err(e) = ipt.flush_chain(IP_TABLE, CUSTOM_CHAIN) {
                errors.push(format!("flush chain: {e}"));
            }
        }

        let jump_rule = format!("-j {CUSTOM_CHAIN}");
        if ipt
            .exists(IP_TABLE, IP_CHAIN, &jump_rule)
            .unwrap_or(false)
        {
            if let Err(e) = ipt.delete(IP_TABLE, IP_CHAIN, &jump_rule) {
                errors.push(format!("delete jump rule: {e}"));
            }
        }

        if ipt
            .chain_exists(IP_TABLE, CUSTOM_CHAIN)
            .unwrap_or(false)
        {
            if let Err(e) = ipt.delete_chain(IP_TABLE, CUSTOM_CHAIN) {
                errors.push(format!("delete chain: {e}"));
            }
        }

        // NAT cleanup
        let nat_rule =
            format!("-m mark --mark {fw_mark} -o {wan_if_name} -j SNAT --to-source {snat_ip}");
        if ipt
            .exists(NAT_TABLE, NAT_CHAIN, &nat_rule)
            .unwrap_or(false)
        {
            if let Err(e) = ipt.delete(NAT_TABLE, NAT_CHAIN, &nat_rule) {
                errors.push(format!("delete NAT SNAT rule: {e}"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(Error::IpTables(errors.join("; ")))
        }
    }

    fn cleanup_stale_rules(&self, fw_mark: u32) -> Result<(), Error> {
        let ipt = &self.inner;

        // Clean up stale mangle chain
        if ipt
            .chain_exists(IP_TABLE, CUSTOM_CHAIN)
            .unwrap_or(false)
        {
            tracing::info!("found stale {} chain - cleaning up", CUSTOM_CHAIN);
            let _ = ipt.flush_chain(IP_TABLE, CUSTOM_CHAIN);
            let jump_rule = format!("-j {CUSTOM_CHAIN}");
            let _ = ipt.delete(IP_TABLE, IP_CHAIN, &jump_rule);
            let _ = ipt.delete_chain(IP_TABLE, CUSTOM_CHAIN);
        }

        // Clean up stale NAT rules by scanning for our mark pattern
        if let Ok(rules) = ipt.list(NAT_TABLE, NAT_CHAIN) {
            let mark_pattern = format!("--mark {fw_mark:#x}");
            let mark_pattern_alt = format!("--mark {fw_mark}");
            for rule in rules {
                if rule.contains(&mark_pattern) || rule.contains(&mark_pattern_alt) {
                    tracing::info!("found stale NAT rule - cleaning up: {}", rule);
                    let _ = ipt.delete(NAT_TABLE, NAT_CHAIN, &rule);
                }
            }
        }

        Ok(())
    }
}
