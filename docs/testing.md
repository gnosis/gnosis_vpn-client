# Testing
In order to test the binary, some functionality can be overriden. Typically, this is done by injecting a custom `hopr` configuration or overriding environment variables.

## Environment overrides
- `GNOSISVPN_HOPR_CONFIG_PATH` - path to a YAML file with the contents of the `hopr` configuration
- `GNOSISVPN_HOPR_IDENTITY_FILE` - path to an explicit identity file to be used 
- `GNOSISVPN_HOPR_IDENTITY_PASS` - string of a password used to unlock the identity file


## Procedure
1. prepare the configuration and load the overrides into the environment
2. run the gnosis_vpn daemon application: `sudo RUST_BACKTRACE=1 RUST_LOG=info ./target/debug/gnosis_vpn -c ~/.fun/gnosis/gvpn-staging.toml --hopr-config-path ~/.config/test/hopr/rotsee/conf/hopr.cfg.yaml` 
