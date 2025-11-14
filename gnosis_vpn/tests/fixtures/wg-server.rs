pub async fn run_wg_server() -> anyhow::Result<()> {
    // pushd modules/gnosis_vpn-server
    //     just docker-build
    //     just docker-run
    // popd

    // # 2b: wait for server
    // EXPECTED_PATTERN="Rocket has launched"
    // TIMEOUT_S=$((60 * 5)) # 5 minutes
    // ENDTIME=$(($(date +%s) + TIMEOUT_S))
    // echo "[PHASE2] Waiting for log '${EXPECTED_PATTERN}' with ${TIMEOUT_S}s timeout"

    // while true; do
    //     if docker logs --since 3s gnosis_vpn-server | grep -q "$EXPECTED_PATTERN"; then
    //         echo "[PHASE2] ${EXPECTED_PATTERN}"
    //         break
    //     fi
    //     if [ $(date +%s) -gt $ENDTIME ]; then
    //         echo "[PHASE2] Timeout reached"
    //         docker logs --tail 20 gnosis_vpn-server
    //         exit 2
    //     fi
    //     sleep 2.5
    // done

    let vpn_server_binary_url =
        "https://github.com/gnosis/gnosis_vpn-server/releases/download/latest/gnosis_vpn-server-aarch64-linux";

    let output = tokio::process::Command::new("curl")
        .arg("-L")
        .arg("-o")
        .arg("/tmp/gnosis_vpn-server")
        .arg(vpn_server_binary_url)
        .output()
        .await?;

    Ok(())
}
