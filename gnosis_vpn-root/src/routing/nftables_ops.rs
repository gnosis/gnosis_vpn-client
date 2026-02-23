//! Firewall rule management abstraction for fwmark-based routing bypass.
//!
//! Defines [`NfTablesOps`] trait for logical firewall operations
//! (setup fwmark rules, teardown, cleanup stale rules).
//!
//! The production implementation uses native nftables via `nftnl` + `mnl`
//! for CLI-free, atomic batch operations through netlink.
//!
//! Production code uses [`RealNfTablesOps`].
//! Tests use stateful mocks (see `mocks` module).

use std::ffi::CString;
use std::net::Ipv4Addr;

use nftnl::expr::{self, Immediate, Nat, NatType, Register};
use nftnl::nft_expr;
use nftnl::{Batch, Chain, ChainType, Hook, MsgType, ProtoFamily, Rule, Table};

use super::Error;

/// Firewall mark used to identify HOPR traffic for bypass routing.
pub(crate) const FW_MARK: u32 = 0xFEED_CAFE;

/// nftables table name for all gnosis_vpn rules.
const TABLE_NAME: &std::ffi::CStr = c"gnosis_vpn";

/// Chain name for output mangle (fwmark setting).
const MANGLE_CHAIN_NAME: &std::ffi::CStr = c"GNOSIS_VPN";

/// Chain name for NAT (SNAT).
const NAT_CHAIN_NAME: &std::ffi::CStr = c"GNOSIS_VPN_NAT";

/// Higher-level abstraction over firewall rule management for fwmark bypass.
///
/// Operates at the logical level: "set up all fwmark rules" / "tear down everything"
/// rather than individual chain and rule manipulation.
pub trait NfTablesOps: Send + Sync {
    /// Set up all firewall rules needed for fwmark-based routing bypass.
    ///
    /// Creates a single nftables table with:
    /// - A route chain to mark traffic from `vpn_uid` with `fw_mark`
    /// - A NAT chain with SNAT rule to rewrite source address for marked traffic
    fn setup_fwmark_rules(&self, vpn_uid: u32, wan_if_name: &str, fw_mark: u32, snat_ip: Ipv4Addr)
    -> Result<(), Error>;

    /// Tear down all firewall rules for fwmark bypass.
    ///
    /// Deletes the entire nftables table, which cascades to all chains and rules.
    fn teardown_rules(&self, wan_if_name: &str, fw_mark: u32, snat_ip: Ipv4Addr) -> Result<(), Error>;

    /// Clean up stale fwmark rules from a previous crash.
    ///
    /// Attempts to delete the nftables table, ignoring ENOENT if it doesn't exist.
    fn cleanup_stale_rules(&self, fw_mark: u32) -> Result<(), Error>;
}

/// Production [`NfTablesOps`] backed by native nftables via `nftnl` + `mnl`.
///
/// Uses a single `gnosis_vpn` nftables table with atomic batch operations.
/// Table-level deletion cascades to all chains and rules, simplifying teardown.
pub struct RealNfTablesOps;

impl RealNfTablesOps {
    pub fn new() -> Result<Self, Error> {
        Ok(Self)
    }
}

/// Sends a finalized nftnl batch over a netlink socket and processes ACKs.
fn send_batch(batch: &nftnl::FinalizedBatch) -> Result<(), Error> {
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

/// Sends a batch that deletes the gnosis_vpn table (cascading all rules).
/// If `ignore_enoent` is true, ENOENT errors are silently ignored.
fn delete_table(ignore_enoent: bool) -> Result<(), Error> {
    let table = Table::new(TABLE_NAME, ProtoFamily::Ipv4);
    let mut batch = Batch::new();
    batch.add(&table, MsgType::Del);
    let finalized = batch.finalize();

    match send_batch(&finalized) {
        Ok(()) => Ok(()),
        Err(ref e) if ignore_enoent => {
            let msg = format!("{e}");
            // ENOENT manifests as "No such file or directory" in the error message
            if msg.contains("No such file or directory") || msg.contains("ENOENT") {
                tracing::debug!("gnosis_vpn table does not exist, nothing to delete");
                Ok(())
            } else {
                Err(Error::NfTables(msg))
            }
        }
        Err(e) => Err(e),
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
        // Delete existing table first (idempotent for crash recovery)
        let _ = delete_table(true);

        let mut batch = Batch::new();

        // Create table
        let table = Table::new(TABLE_NAME, ProtoFamily::Ipv4);
        batch.add(&table, MsgType::Add);

        // Create mangle chain (Route type, Output hook)
        // Route type is used to reroute packets when marks are modified
        let mut mangle_chain = Chain::new(MANGLE_CHAIN_NAME, &table);
        mangle_chain.set_hook(Hook::Out, 0);
        mangle_chain.set_type(ChainType::Route);
        batch.add(&mangle_chain, MsgType::Add);

        // Rule 1: loopback return — skip marking for loopback traffic
        let mut lo_rule = Rule::new(&mangle_chain);
        lo_rule.add_expr(&nft_expr!(meta oifname));
        lo_rule.add_expr(&nft_expr!(
            cmp == expr::InterfaceName::Exact(CString::new("lo").unwrap())
        ));
        lo_rule.add_expr(&nft_expr!(verdict return));
        batch.add(&lo_rule, MsgType::Add);

        // Rule 2: uid match + set mark
        // Match packets from the VPN worker UID, then set the firewall mark
        let mut uid_rule = Rule::new(&mangle_chain);
        uid_rule.add_expr(&nft_expr!(meta skuid));
        uid_rule.add_expr(&nft_expr!(cmp == vpn_uid));
        // Load the mark value into register 1, then apply it via meta mark set
        uid_rule.add_expr(&Immediate::new(fw_mark, Register::Reg1));
        uid_rule.add_expr(&nft_expr!(meta mark set));
        batch.add(&uid_rule, MsgType::Add);

        // Create NAT chain (PostRouting hook)
        let mut nat_chain = Chain::new(NAT_CHAIN_NAME, &table);
        nat_chain.set_hook(Hook::PostRouting, 100);
        nat_chain.set_type(ChainType::Nat);
        batch.add(&nat_chain, MsgType::Add);

        // Rule 3: mark match + oifname match + SNAT
        // For marked traffic going out the WAN interface, rewrite the source address
        let wan_if_cstr =
            CString::new(wan_if_name).map_err(|e| Error::NfTables(format!("invalid WAN interface name: {e}")))?;

        let mut snat_rule = Rule::new(&nat_chain);
        snat_rule.add_expr(&nft_expr!(meta mark));
        snat_rule.add_expr(&nft_expr!(cmp == fw_mark));
        snat_rule.add_expr(&nft_expr!(meta oifname));
        snat_rule.add_expr(&nft_expr!(cmp == expr::InterfaceName::Exact(wan_if_cstr)));
        // Load the SNAT IP into register 1, then apply NAT
        snat_rule.add_expr(&Immediate::new(snat_ip, Register::Reg1));
        snat_rule.add_expr(&Nat {
            nat_type: NatType::SNat,
            family: ProtoFamily::Ipv4,
            ip_register: Register::Reg1,
            port_register: None,
        });
        batch.add(&snat_rule, MsgType::Add);

        let finalized = batch.finalize();
        send_batch(&finalized)?;

        tracing::debug!(
            "nftables rules set up: table={}, uid={vpn_uid}, mark={fw_mark:#x}, snat={snat_ip}, wan={wan_if_name}",
            TABLE_NAME.to_string_lossy()
        );

        Ok(())
    }

    fn teardown_rules(&self, _wan_if_name: &str, _fw_mark: u32, _snat_ip: Ipv4Addr) -> Result<(), Error> {
        // Deleting the table cascades to all chains and rules
        delete_table(false)
    }

    fn cleanup_stale_rules(&self, _fw_mark: u32) -> Result<(), Error> {
        // Attempt to delete table, ignore if it doesn't exist
        delete_table(true)
    }
}
