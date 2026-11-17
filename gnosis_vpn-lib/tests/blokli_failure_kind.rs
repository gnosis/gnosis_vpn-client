//! The worker's first Blokli call, against a closed port, must classify as unreachable for the sync screen.
use edgli::blokli::make_incentive_operations;
use edgli::hopr_lib::builder::{ChainKeypair, Keypair};
use edgli::{BlokliEndpoint, FailureKind, Url};

async fn closed_port() -> Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    format!("http://{addr}").parse().expect("valid URL")
}

#[tokio::test]
async fn a_refused_connection_is_unreachable() {
    let endpoint = BlokliEndpoint::new(closed_port().await);
    let chain_key = ChainKeypair::random();

    let Err(err) = make_incentive_operations(endpoint, &chain_key, None).await else {
        panic!("a closed port must not yield a working handle");
    };

    assert_eq!(err.failure_kind(), FailureKind::Unreachable, "{err}");
}
