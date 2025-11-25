# Migration Plan: Safe and Identity Logic to edge-client

## Overview

This document outlines the plan to migrate safe module deployment and identity management functionality from the gnosis_vpn-client repository to the hoprnet/edge-client repository.

## Motivation

The safe module deployment and identity management logic is generic HOPR functionality that should be available in the edge-client library. Moving this code to edge-client will:

1. **Enable reuse**: Other HOPR applications can use this functionality
2. **Improve testability**: Logic can be tested independently of VPN-specific code
3. **Reduce duplication**: Avoid maintaining similar code in multiple repositories
4. **Better separation of concerns**: VPN client focuses on VPN functionality, edge-client handles HOPR infrastructure

## Code to Migrate

### 1. Safe Module Deployment (`edge_contrib/safe_deployment.rs`)

**Functionality:**
- Deploy Safe modules on HOPR networks
- Build deployment transaction data
- Parse deployment results from blockchain events

**Key Components:**
- `SafeDeploymentConfig`: Configuration for deploying a Safe module
- `SafeDeploymentResult`: Result of a Safe deployment
- `NetworkConfig`: Network-specific contract addresses
- `SafeDeployer`: Main deployer interface

**Current Location in VPN Client:**
- `gnosis_vpn-lib/src/chain/contracts.rs` (lines 141-219)
- `gnosis_vpn-lib/src/chain/constants.rs` (constants)

**Proposed Location in edge-client:**
- `hopr-lib/src/safe/deployment.rs`

**Dependencies:**
- `alloy` (already used in edge-client)
- Contract ABIs (Token, HoprNodeStakeFactory)

**Tests:**
- Unit tests for user data encoding
- Unit tests for default target building
- Integration tests for full deployment flow (requires test network)

### 2. Identity Management (`edge_contrib/identity.rs`)

**Functionality:**
- Load HOPR node identities from files
- Generate secure passwords for identity files
- Create new identities

**Key Components:**
- `IdentityConfig`: Configuration for identity files
- `IdentityManager`: Main interface for identity operations
- `IdentityError`: Error types for identity operations

**Current Location in VPN Client:**
- `gnosis_vpn-lib/src/hopr/identity.rs` (file path management)
- Identity loading already uses `HoprKeys` and `IdentityRetrievalModes` from edge-client

**Proposed Location in edge-client:**
- `hopr-lib/src/identity/manager.rs`

**Dependencies:**
- `rand` (for password generation)
- Already depends on `HoprKeys` and `IdentityRetrievalModes` from hopr-lib

**Tests:**
- Unit tests for password generation
- Unit tests for identity creation
- Unit tests for identity loading
- Integration tests for full roundtrip (create, save, load)

### 3. Safe Module Persistence (Additional Functionality)

**Functionality:**
- Store Safe module configuration to disk
- Load Safe module configuration from disk
- Check if Safe module exists

**Current Location in VPN Client:**
- `gnosis_vpn-lib/src/hopr/config.rs` (lines 53-61, 120-122)

**Proposed Location in edge-client:**
- `hopr-lib/src/safe/persistence.rs`

**Key Components:**
```rust
pub struct SafeModulePersistence;

impl SafeModulePersistence {
    pub async fn store(safe_module: &SafeModule, path: &Path) -> Result<(), Error>;
    pub async fn load(path: &Path) -> Result<SafeModule, Error>;
    pub fn exists(path: &Path) -> bool;
}
```

## Migration Strategy

### Phase 1: Preparation (Current)
- [x] Extract safe deployment logic into `edge_contrib/safe_deployment.rs`
- [x] Extract identity management logic into `edge_contrib/identity.rs`
- [x] Add comprehensive unit tests
- [ ] Run tests to ensure all functionality works
- [ ] Document public APIs

### Phase 2: Contribution to edge-client
1. Create PR to edge-client with:
   - New modules: `hopr-lib/src/safe/` and `hopr-lib/src/identity/`
   - Tests for all new functionality
   - Documentation for public APIs
   - Examples of usage

2. Wait for PR review and merge

### Phase 3: Update VPN Client
1. Update edge-client dependency to include new functionality
2. Replace local implementations with edge-client imports:
   - Replace `edge_contrib::safe_deployment` with `edgli::hopr_lib::safe`
   - Replace `edge_contrib::identity` with `edgli::hopr_lib::identity`
3. Update all call sites
4. Remove `edge_contrib` module
5. Run full test suite

### Phase 4: Cleanup
1. Remove old implementations from VPN client
2. Update documentation
3. Verify all functionality still works

## API Contracts

### Safe Deployment

```rust
// In edge-client: edgli::hopr_lib::safe

pub struct NetworkConfig {
    pub channels_contract_address: Address,
    pub node_stake_factory_address: Address,
}

pub struct SafeDeploymentConfig {
    pub token_amount: U256,
    pub nonce: U256,
    pub admins: Vec<Address>,
}

pub struct SafeDeploymentResult {
    pub tx_hash: B256,
    pub safe_address: Address,
    pub module_address: Address,
}

impl SafeDeployer {
    pub async fn deploy<P>(
        provider: &P,
        config: &SafeDeploymentConfig,
        network_config: &NetworkConfig,
    ) -> Result<SafeDeploymentResult, SafeDeploymentError>
    where P: Provider + Clone;
}
```

### Identity Management

```rust
// In edge-client: edgli::hopr_lib::identity

pub struct IdentityConfig {
    pub file_path: PathBuf,
    pub password: String,
}

impl IdentityManager {
    pub fn load_from_file(file: &Path, password: String) -> Result<HoprKeys, IdentityError>;
    pub fn load_from_config(config: &IdentityConfig) -> Result<HoprKeys, IdentityError>;
    pub fn generate_password() -> String;
    pub fn create_new(file_path: &Path) -> Result<(HoprKeys, String), IdentityError>;
}
```

## Testing Requirements

### Safe Deployment Tests
- [x] Unit test: `build_user_data` produces correct ABI encoding
- [x] Unit test: `build_default_target` combines address and suffix correctly
- [x] Unit test: `SafeDeploymentConfig::new` creates valid configuration
- [ ] Integration test: Full deployment on test network
- [ ] Integration test: Event parsing from deployment transaction

### Identity Management Tests
- [x] Unit test: `generate_password` produces 48-character alphanumeric passwords
- [x] Unit test: `generate_password` produces unique passwords
- [x] Unit test: `create_new` creates valid identity
- [x] Unit test: `load_from_file` fails on non-existent file
- [x] Integration test: Roundtrip create and load identity
- [ ] Integration test: Load identity created by hoprnet CLI

### Safe Persistence Tests
- [ ] Unit test: `store` writes valid YAML
- [ ] Unit test: `load` reads valid YAML
- [ ] Unit test: `exists` correctly detects file presence
- [ ] Integration test: Store and load roundtrip

## VPN Client Integration Points

After migration, the VPN client will use edge-client functionality at these points:

1. **Initial setup** (`core/mod.rs`):
   ```rust
   use edgli::hopr_lib::identity::IdentityManager;
   use edgli::hopr_lib::safe::SafeDeployer;
   ```

2. **Identity loading** (`hopr_params.rs`):
   ```rust
   let keys = IdentityManager::load_from_config(&config)?;
   ```

3. **Safe deployment** (`core/runner.rs`):
   ```rust
   let result = SafeDeployer::deploy(&provider, &config, &network_config).await?;
   ```

## Benefits

1. **Reduced code duplication**: ~500 lines of code moved to shared library
2. **Better testing**: Logic can be tested independently
3. **Easier maintenance**: Single source of truth for safe and identity logic
4. **Reusability**: Other HOPR applications can use this functionality
5. **Cleaner VPN client**: VPN-specific code separated from HOPR infrastructure

## Risks and Mitigation

1. **Risk**: Breaking changes in edge-client affect VPN client
   - **Mitigation**: Pin to specific edge-client version, update carefully

2. **Risk**: Migration introduces bugs
   - **Mitigation**: Comprehensive test coverage before and after migration

3. **Risk**: Different requirements between VPN client and edge-client
   - **Mitigation**: Design generic APIs that work for both use cases

## Timeline

- Phase 1 (Preparation): 1-2 days
- Phase 2 (edge-client PR): 3-5 days (including review)
- Phase 3 (VPN client update): 1-2 days
- Phase 4 (Cleanup): 1 day

Total: ~1-2 weeks
