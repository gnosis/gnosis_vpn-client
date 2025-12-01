    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail


    : "${SYSTEM_TEST_HOPRD_ID:?SYSTEM_TEST_HOPRD_ID must be set to run system tests}"
    : "${SYSTEM_TEST_HOPRD_ID_PASSWORD:?SYSTEM_TEST_HOPRD_ID_PASSWORD must be set to run system tests}"
    : "${SYSTEM_TEST_SAFE:?SYSTEM_TEST_SAFE must be set to run system tests}"

    config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/gnosisvpn"
    mkdir -p "${config_dir}"

    printf %s "${SYSTEM_TEST_HOPRD_ID}" > "${config_dir}/gnosisvpn-hopr.id"
    printf %s "${SYSTEM_TEST_HOPRD_ID_PASSWORD}" > "${config_dir}/gnosisvpn-hopr.pass"
    printf %s "${SYSTEM_TEST_SAFE}" > "${config_dir}/gnosisvpn-hopr.safe"

    gnosis_vpn-system-tests download