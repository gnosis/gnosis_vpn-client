use gnosis_vpn_lib::hopr::hopr_lib::testing::fixtures::{ClusterGuard, cluster_fixture, exclusive_indexes};
use rstest::rstest;

#[rstest]
#[tokio::test]
async fn test_dummy_test(#[future(awt)] cluster_fixture: ClusterGuard) -> anyhow::Result<()> {
    let [idx] = exclusive_indexes::<1>();

    Ok(())
}
