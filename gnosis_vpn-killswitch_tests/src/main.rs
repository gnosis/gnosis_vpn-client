//! Integration test for the nftables killswitch.
//!
//! NOT a `cargo test` target — run via `just killswitch-test` or the CI job.
//! Requires root / CAP_NET_ADMIN to manipulate nftables.
//!
//! Test sequence:
//!   1. ping 1.1.1.1 → success (baseline)
//!   2. apply_policy(&[]) → lockdown (block everything)
//!   3. ping 1.1.1.1 → failure expected
//!   4. reset_policy() → restore (always runs before any assertion)
//!   5. ping 1.1.1.1 → success again

#[cfg(target_os = "linux")]
fn main() {
    use gnosis_vpn_lib::killswitch::Firewall;
    use std::net::IpAddr;
    use std::process::Command;

    fn ping(label: &str) -> bool {
        let ok = Command::new("ping")
            .args(["-c1", "-W2", "1.1.1.1"])
            .status()
            .expect("failed to run ping")
            .success();
        eprintln!("[killswitch-test] {label}: {}", if ok { "SUCCESS" } else { "FAILED" });
        ok
    }

    fn check(label: &str, ok: bool) {
        if !ok {
            eprintln!("[killswitch-test] ASSERTION FAILED: {label}");
            std::process::exit(1);
        }
    }

    let mut fw = Firewall::new();

    check("baseline ping", ping("pre-lockdown"));

    fw.apply_policy(&[] as &[IpAddr])
        .expect("failed to apply killswitch policy");
    eprintln!("[killswitch-test] lockdown applied");

    let blocked_while_locked = !ping("locked-down");

    // Always reset before asserting — don't leave the host firewalled on failure
    fw.reset_policy().expect("failed to reset killswitch policy");
    eprintln!("[killswitch-test] lockdown reset");

    check("ping blocked during lockdown", blocked_while_locked);
    check("post-reset ping", ping("post-reset"));

    eprintln!("[killswitch-test] ALL PASSED");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("killswitch integration test is Linux-only");
    std::process::exit(1);
}
