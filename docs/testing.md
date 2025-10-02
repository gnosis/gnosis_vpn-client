# Testing
In order to test the binary, some functionality can be overriden. Typically, this is done by injecting a custom `hopr` configuration or overriding environment variables.

## Environment overrides
The following variables can be used to override the default gnosis_vpn-client behavior:
- `GNOSISVPN_HOPR_CONFIG_PATH` - path to a YAML file with the contents of the `hopr` configuration

  - the configuration must hold the `HoprLibConfig` format
- `GNOSISVPN_HOPR_IDENTITY_FILE` - path to an explicit identity file to be used 
- `GNOSISVPN_HOPR_IDENTITY_PASS` - string of a password used to unlock the identity file


## Procedure
1. Prepare a directory structure:
```
├── conf
│   ├── hopr.cfg.yaml
│   └── hopr.id
├── data
│   └── db
│       ...
└── hopr.id.pass
```
2. Modify the configuration inside the `hopr.cfg.yaml` based on your requirements:
```yaml
host:
  address: !Domain 45u1451o2i45124512345.example.com    # anything really, will be announced, but cannot be contacted
  port: 9090  # local port for p2p
db:
  data: <PATH_TO_CONFIG_DIR_STRUCTURE>/data/
chain:
  network: rotsee
  provider: https://gnosis-rpc.publicnode.com
  announce: true
safe_module:
  safe_address: <SAFE ADDRESS>
  module_address: <MODULE ADDRESS>
```

3. Prepare a `rotsee` testing network minimal configuration `gvpn-staging.toml`:
```toml
  version = 4
  
  # 1-hop USA
  [destinations.0x7220CfE91F369bfE79F883c2891e97407D7a4D48]
  meta = { location = "USA", state = "Iowa" }
  path = { intermediates = [ "0xFE3AF421afB84EED445c2B8f1892E3984D3e41eA" ] }
  # path = { hops = 0 }

  # 0-hop Europe
  [destinations.0xcD9D0E23cD999dFC0D1400D837F8e612DbbbDFAA]
  meta = { location = "UK", city = "London" }
  # path = { intermediates = [ "0xc00B7d90463394eC29a080393fF09A2ED82a0F86" ] }
  path = { hops = 0 }
```
4. run the gnosis_vpn daemon application:
```shell
sudo RUST_BACKTRACE=1 RUST_LOG=info gnosis_vpn -c <PATH_TO_VPN_FILES>/gvpn-staging.toml --hopr-config-path <PATH_TO_CONFIG_DIR_STRUCTURE>/conf/hopr.cfg.yaml
```

5. check the status:
```shell
gnosis_vpn-ctl status
```

6. connect to e.g. London:
```shell
gnosis_vpn-ctl connect 0xcD9D0E23cD999dFC0D1400D837F8e612DbbbDFAA
```
