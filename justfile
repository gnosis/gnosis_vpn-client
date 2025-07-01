# build static linux binary
build:
    nix build .#gnosisvpn-x86_64-linux

# build docker image
docker-build: build
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail

    cp result/bin/* docker/
    chmod 775 docker/gnosis_vpn docker/gnosis_vpn-ctl
    docker build --platform linux/x86_64 -t gnosis_vpn-client docker/

# run docker container detached
docker-run:
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail

    log_level=$(if [ "${RUST_LOG:-}" = "" ]; then echo info; else echo "${RUST_LOG}"; fi)

    docker run --detach --rm \
        --env DESTINATION_ADDRESS_1=${DESTINATION_ADDRESS_1} \
        --env DESTINATION_ADDRESS_2=${DESTINATION_ADDRESS_2} \
        --env API_PORT=${API_PORT} \
        --env API_TOKEN=${API_TOKEN} \
        --env RUST_LOG=${log_level} \
        --publish 51822:51820/udp \
        --cap-add=NET_ADMIN \
        --add-host=host.docker.internal:host-gateway \
        --sysctl net.ipv4.conf.all.src_valid_mark=1 \
        --name gnosis_vpn-client gnosis_vpn-client

# stop docker container
docker-stop:
    docker stop gnosis_vpn-client

# enter docker container interactively
docker-enter:
    docker exec --interactive --tty gnosis_vpn-client bash

# checkout submodules
submodules:
    git submodule update --init --force

# helper to start local cluster from hoprnet submodule
start-cluster:
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail
    # if on github hosted runner try to free some extra space (see https://github.com/orgs/community/discussions/25678)
    rm -rf /opt/hostedtoolcache

    cd modules/hoprnet
    nix develop .#citest --command make localcluster-exposed

# full system setup with system tests: 'mode' can be either 'keep-running' or 'ci-system-test'
system-setup mode='keep-running': submodules docker-build
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail

    cleanup() {
        echo "[CLEANUP] Shutting down cluster"
        # Send SIGINT to the entire process group (negative PID)
        timeout --kill-after=1m 30s kill -INT -- -$CLUSTER_PID

        echo "[CLEANUP] Shutting down server container"
        just docker-stop || true

        echo "[CLEANUP] Shutting down client container"
        cd modules/gnosis_vpn-server && just docker-stop || true

        echo "[CLEANUP] Done"
    }

    trap cleanup SIGINT SIGTERM EXIT


    ####
    ## PHASE 1: ready local cluster

    # 1a: start cluster
    setsid just start-cluster > cluster.log 2>&1 &
    CLUSTER_PID=$!
    echo "[PHASE1] Starting cluster with PID: $CLUSTER_PID"

    # 1b: wait for nodes
    EXPECTED_PATTERN="All nodes ready"
    TIMEOUT_S=$((60 * 50)) # 50 minutes
    ENDTIME=$(($(date +%s) + TIMEOUT_S))
    echo "[PHASE1] Waiting for log '${EXPECTED_PATTERN}' with ${TIMEOUT_S}s timeout"

    # print progress report each minute
    ONGOING_INTERVAL_S=60
    START_TIME=$(date +%s)
    NEXT_REPORT_TIME=$((START_TIME + ONGOING_INTERVAL_S))

    while true; do
        if grep -q "$EXPECTED_PATTERN" cluster.log; then
            echo "[PHASE1] ${EXPECTED_PATTERN}"
            # even though nodes are ready the output was not yet printed - waiting for a bit
            sleep 3
            break
        fi
        if [ $(date +%s) -gt $ENDTIME ]; then
            echo "[PHASE1] Timeout reached"
            tail --lines 50 cluster.log
            exit 1
        fi
        if [ $(date +%s) -gt $NEXT_REPORT_TIME ]; then
            NEXT_REPORT_TIME=$((NEXT_REPORT_TIME + ONGOING_INTERVAL_S))
            ELAPSED_TIME=$(($(date +%s) - $START_TIME))
            echo "[PHASE1] Peek cluster log after $((ELAPSED_TIME / 60)) minutes"
            tail --lines 5 cluster.log
        fi
        sleep 1
    done

    # 1c: extract values
    echo "[PHASE1] Extracting values from cluster log"
    ADDRESS_LOCAL5=$(grep "Address:" cluster.log | tail -n 2 | head -n 1 | awk '{print $2}')
    ADDRESS_LOCAL6=$(grep "Address:" cluster.log | tail -n 1 | awk '{print $2}')
    API_PORT_LOCAL1=3003
    API_TOKEN_LOCAL1=$(grep -A3 -P "^\tnode @ .*:$API_PORT_LOCAL1" cluster.log | tail -n 1 | sed -E 's#.*apiToken=([^&]+).*#\1#')

    echo "[PHASE1] ADDRESS 1 (local5): $ADDRESS_LOCAL5"
    echo "[PHASE1] ADDRESS 2 (local6): $ADDRESS_LOCAL6"
    echo "[PHASE1] API Token (local1): $API_TOKEN_LOCAL1"
    echo "[PHASE1] API Port (local1): $API_PORT_LOCAL1"


    ####
    ## PHASE 2: ready gnosis_vpn-server

    # 2a: start server
    echo "[PHASE2] Starting gnosis_vpn-server"
    pushd modules/gnosis_vpn-server
        just docker-build
        just docker-run
    popd

    # 2b: wait for server
    EXPECTED_PATTERN="Rocket has launched"
    TIMEOUT_S=$((60 * 5)) # 5 minutes
    ENDTIME=$(($(date +%s) + TIMEOUT_S))
    echo "[PHASE2] Waiting for log '${EXPECTED_PATTERN}' with ${TIMEOUT_S}s timeout"

    while true; do
        if docker logs --since 3s gnosis_vpn-server | grep -q "$EXPECTED_PATTERN"; then
            echo "[PHASE2] ${EXPECTED_PATTERN}"
            break
        fi
        if [ $(date +%s) -gt $ENDTIME ]; then
            echo "[PHASE2] Timeout reached"
            docker logs --tail 20 gnosis_vpn-server
            exit 2
        fi
        sleep 2.5
    done

    echo "[PHASE2] Server is ready for testing"

    ####
    ## PHASE 3: ready gnosis_vpn-client

    # 3a: start client
    echo "[PHASE3] Starting gnosis_vpn-client"
    # container was build as part of the deps
    DESTINATION_ADDRESS_1="${ADDRESS_LOCAL5}" \
        DESTINATION_ADDRESS_2="${ADDRESS_LOCAL6}" \
        API_TOKEN="${API_TOKEN_LOCAL1}" \
        API_PORT="${API_PORT_LOCAL1}" \
        just docker-run

    exp_client_log() {
        EXPECTED_PATTERN="$1"
        TIMEOUT_S="${2}"
        ENDTIME=$(($(date +%s) + TIMEOUT_S))
        echo "[PHASE3] Waiting for log '${EXPECTED_PATTERN}' with ${TIMEOUT_S}s timeout"

        while true; do
            if docker logs --since 3s gnosis_vpn-client | grep -q "$EXPECTED_PATTERN"; then
                echo "[PHASE3] ${EXPECTED_PATTERN}"
                break
            fi
            if [ $(date +%s) -gt $ENDTIME ]; then
                echo "[PHASE3] Timeout reached"
                docker logs --tail 20 gnosis_vpn-client
                return 1
            fi
            sleep 2.5
        done
    }

    # 3b: wait for client to be ready
    exp_client_log "enter listening mode" 33
    echo "[PHASE3] Client is ready for testing"

    # 3c: run system tests
    if [ "{{ mode }}" = "ci-system-test" ]; then
        echo "[PHASE3] Checking connect via first local node"
        docker exec gnosis_vpn-client ./gnosis_vpn-ctl connect ${ADDRESS_LOCAL5}
        exp_client_log "VPN CONNECTION ESTABLISHED" 33
        echo "[PHASE3] Checking working ping first node"
        exp_client_log "Session verified as open" 33

        echo "[PHASE3] Checking ping"
        ping -c1 10.128.0.1 || (
            docker logs gnosis_vpn-client
            exit 1
        )

        echo "[PHASE3] Waiting for 40 seconds to ensure connection stability"
        sleep 40

        echo "[PHASE3] Checking ping again"
        ping -c1 10.128.0.1 || (
            docker logs gnosis_vpn-client
            exit 1
        )

        echo "[PHASE3] Checking connect via second local node"
        docker exec gnosis_vpn-client ./gnosis_vpn-ctl connect ${ADDRESS_LOCAL6}
        exp_client_log "VPN CONNECTION ESTABLISHED" 33
        echo "[PHASE3] Checking working ping second node"
        exp_client_log "Session verified as open" 33

        echo "[PHASE3] Checking disconnect"
        docker exec gnosis_vpn-client ./gnosis_vpn-ctl disconnect
        exp_client_log "WireGuard connection closed" 33

        echo "[PHASE3] Checking no warnings or errors in client logs"
        if docker logs gnosis_vpn-client | grep -qE "WARN|ERROR"; then
            echo "[PHASE3] Found warnings or errors in client logs"
            docker logs gnosis_vpn-client
            exit 1
        fi
        echo "[PHASE3] Checking no errors in server logs"
        if docker logs gnosis_vpn-server | grep -qE "ERROR"; then
            echo "[PHASE3] Found errors in server logs"
            docker logs gnosis_vpn-server
            exit 1
        fi
        echo "[SUCCESS] System test completed successfully"
        exit 0
    else
        echo "[PHASE3] System setup complete, keeping components running"
        echo "[PHASE3] Press Ctrl+C to stop the cluster and containers"
        wait $CLUSTER_PID
        exit 0
    fi
