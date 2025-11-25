# Edge Contrib Module

This module contains functionality that is intended to be contributed to the `hoprnet/edge-client` repository.

## Purpose

The code in this module has been extracted from the VPN client to:
1. Make it reusable across multiple HOPR applications
2. Enable isolated testing of HOPR infrastructure functionality
3. Separate VPN-specific logic from general HOPR functionality

## Modules

### `safe_deployment`
Handles deployment of Safe modules on HOPR networks.

**Key Components:**
- `SafeDeployer`: Main interface for deploying Safe modules
- `SafeDeploymentConfig`: Configuration for deployments
- `SafeDeploymentResult`: Result of a deployment
- `NetworkConfig`: Network-specific contract addresses

**Tests:** 4 unit tests covering configuration, user data encoding, and result handling

### `safe_persistence`
Manages persistence of Safe module configurations to disk.

**Key Components:**
- `SafeModulePersistence`: Interface for storing and loading Safe configurations

**Tests:** 5 unit tests covering store, load, existence checks, and error handling

### `identity`
Manages HOPR node identities.

**Key Components:**
- `IdentityManager`: Interface for creating and loading identities
- `IdentityConfig`: Configuration for identity files

**Tests:** 6 unit tests covering password generation, identity creation, and loading

## Migration Path

Once this code is merged into edge-client:

1. The edge-client dependency will be updated in the VPN client
2. Imports will change from `gnosis_vpn_lib::edge_contrib::*` to `edgli::hopr_lib::*`
3. This module will be deleted

See [MIGRATION.md](../../MIGRATION.md) for detailed migration plan.

## Test Coverage

Total: 15 unit tests
- Safe deployment: 4 tests
- Safe persistence: 5 tests  
- Identity management: 6 tests

All tests focus on the public API and core functionality. Integration tests with actual blockchain interactions are not included as they would require test networks and are better suited for the edge-client repository.

## Dependencies

- `alloy`: Ethereum library for contract interactions
- `edgli::hopr_lib`: Core HOPR types (HoprKeys, SafeModule, etc.)
- `rand`: Random number generation for passwords
- `serde_yaml`: YAML serialization for configurations
- `tokio`: Async runtime
- `thiserror`: Error handling

All dependencies are already used elsewhere in the project.
