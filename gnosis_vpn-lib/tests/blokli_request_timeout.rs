//! End-to-end check that `[blokli] request_timeout` reaches the HTTP client that talks to
//! Blokli.
//!
//! The unit tests cover each hop of the chain in isolation - config file to `BlokliConfig`, to
//! `BlokliEndpoint`, to the connector's client config. This test closes it by pointing the real
//! client at a black-hole listener and timing the failure: a socket that completes the TCP
//! handshake and then never answers is exactly the shape of the high-latency WAN failure this
//! setting exists for.
//!
//! Ignored by default because it spends the timeout in wall-clock time. Run with:
//! `cargo test -p gnosis_vpn-lib --test blokli_request_timeout -- --ignored`

use std::time::{Duration, Instant};

use edgli::blokli::make_incentive_operations;
use edgli::hopr_lib::builder::{ChainKeypair, Keypair};
use edgli::{BlokliEndpoint, Url};

/// Comfortably clear of the 3 s connector default, so a regression that drops the configured
/// value on the floor shows up as a too-early failure rather than a flaky margin.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Accepts connections and then never writes a byte, so every request runs to its timeout.
async fn black_hole() -> Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        let mut accepted = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            // Hold the stream open; dropping it would surface as a connection reset instead.
            accepted.push(stream);
        }
    });

    format!("http://{addr}").parse().expect("valid URL")
}

#[tokio::test]
#[ignore = "spends the configured timeout in wall-clock time"]
async fn a_configured_request_timeout_bounds_a_stalled_blokli_request() {
    let endpoint = BlokliEndpoint::new(black_hole().await).with_request_timeout(REQUEST_TIMEOUT);
    let chain_key = ChainKeypair::random();

    let started = Instant::now();
    let result = make_incentive_operations(endpoint, &chain_key, None).await;
    let elapsed = started.elapsed();

    assert!(result.is_err(), "a black-hole endpoint must not yield a working handle");
    assert!(
        elapsed >= Duration::from_secs(5),
        "gave up after {elapsed:?} - the configured {REQUEST_TIMEOUT:?} was not applied, \
         the connector's own default was"
    );
    assert!(
        elapsed < Duration::from_secs(30),
        "took {elapsed:?} - the request is not bounded by the configured timeout at all"
    );
}
