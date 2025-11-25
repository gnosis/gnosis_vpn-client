# Examples: Using edge_contrib Modules

This document provides examples of how to use the functionality in the `edge_contrib` module.

## Identity Management

### Creating a New Identity

```rust
use gnosis_vpn_lib::edge_contrib::{IdentityManager, IdentityConfig};
use std::path::PathBuf;

async fn create_identity() -> Result<(), Box<dyn std::error::Error>> {
    let id_path = PathBuf::from("/path/to/identity.id");
    
    // Create a new identity with an auto-generated password
    let (keys, password) = IdentityManager::create_new(&id_path)?;
    
    // Save the password to a secure location
    tokio::fs::write("/path/to/identity.pass", password.as_bytes()).await?;
    
    // Use the keys
    let node_address = keys.chain_key.public().to_address();
    println!("Node address: {:?}", node_address);
    
    Ok(())
}
```

### Loading an Existing Identity

```rust
use gnosis_vpn_lib::edge_contrib::{IdentityManager, IdentityConfig};
use std::path::PathBuf;

async fn load_identity() -> Result<(), Box<dyn std::error::Error>> {
    let id_path = PathBuf::from("/path/to/identity.id");
    
    // Read the password
    let password = tokio::fs::read_to_string("/path/to/identity.pass").await?;
    
    // Load the identity
    let keys = IdentityManager::load_from_file(&id_path, password)?;
    
    println!("Identity loaded successfully");
    
    Ok(())
}
```

### Using IdentityConfig

```rust
use gnosis_vpn_lib::edge_contrib::{IdentityManager, IdentityConfig};
use std::path::PathBuf;

fn load_with_config() -> Result<(), Box<dyn std::error::Error>> {
    let config = IdentityConfig::new(
        PathBuf::from("/path/to/identity.id"),
        "my-secret-password".to_string(),
    );
    
    let keys = IdentityManager::load_from_config(&config)?;
    
    Ok(())
}
```

## Safe Deployment

### Deploying a Safe Module

```rust
use gnosis_vpn_lib::edge_contrib::{SafeDeployer, SafeDeploymentConfig, NetworkConfig};
use alloy::primitives::{U256, address};

async fn deploy_safe<P>(provider: &P) -> Result<(), Box<dyn std::error::Error>>
where
    P: alloy::providers::Provider + Clone,
{
    // Configure network (Rotsee example)
    let network_config = NetworkConfig {
        channels_contract_address: address!("0x77C9414043d27fdC98A6A2d73fc77b9b383092a7"),
        node_stake_factory_address: address!("0x439f5457FF58CEE941F7d946CB919c52EA30cfB3"),
    };
    
    // Configure deployment
    let nonce = U256::from(rand::random::<u64>());
    let token_amount = U256::from(1000000000000000000u128); // 1 token
    let admin_address = address!("0x1234567890123456789012345678901234567890");
    
    let deployment_config = SafeDeploymentConfig::new(
        nonce,
        token_amount,
        vec![admin_address],
    );
    
    // Deploy
    let result = SafeDeployer::deploy(provider, &deployment_config, &network_config).await?;
    
    println!("Safe deployed!");
    println!("  Transaction: {:?}", result.tx_hash);
    println!("  Safe address: {:?}", result.safe_address);
    println!("  Module address: {:?}", result.module_address);
    
    Ok(())
}
```

## Safe Persistence

### Storing a Safe Configuration

```rust
use gnosis_vpn_lib::edge_contrib::SafeModulePersistence;
use edgli::hopr_lib::config::SafeModule;
use std::path::PathBuf;
use alloy::primitives::address;

async fn store_safe() -> Result<(), Box<dyn std::error::Error>> {
    let safe_module = SafeModule {
        safe_address: address!("0x1234567890123456789012345678901234567890").into(),
        module_address: address!("0x0987654321098765432109876543210987654321").into(),
        ..Default::default()
    };
    
    let path = PathBuf::from("/path/to/safe.yaml");
    
    SafeModulePersistence::store(&safe_module, &path).await?;
    
    println!("Safe configuration saved to {}", path.display());
    
    Ok(())
}
```

### Loading a Safe Configuration

```rust
use gnosis_vpn_lib::edge_contrib::SafeModulePersistence;
use std::path::PathBuf;

async fn load_safe() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from("/path/to/safe.yaml");
    
    if SafeModulePersistence::exists(&path) {
        let safe_module = SafeModulePersistence::load(&path).await?;
        
        println!("Safe address: {:?}", safe_module.safe_address);
        println!("Module address: {:?}", safe_module.module_address);
    } else {
        println!("Safe configuration not found");
    }
    
    Ok(())
}
```

## Complete Workflow Example

Here's a complete example that combines identity, safe deployment, and persistence:

```rust
use gnosis_vpn_lib::edge_contrib::{
    IdentityManager, IdentityConfig, SafeDeployer, SafeDeploymentConfig,
    NetworkConfig, SafeModulePersistence
};
use alloy::primitives::{U256, address};
use std::path::PathBuf;

async fn complete_setup<P>(provider: &P) -> Result<(), Box<dyn std::error::Error>>
where
    P: alloy::providers::Provider + Clone,
{
    // Step 1: Create or load identity
    let id_path = PathBuf::from("./hopr.id");
    let pass_path = PathBuf::from("./hopr.pass");
    
    let keys = if id_path.exists() {
        let password = tokio::fs::read_to_string(&pass_path).await?;
        IdentityManager::load_from_file(&id_path, password)?
    } else {
        let (keys, password) = IdentityManager::create_new(&id_path)?;
        tokio::fs::write(&pass_path, password.as_bytes()).await?;
        keys
    };
    
    let node_address = keys.chain_key.public().to_address();
    println!("Node address: {:?}", node_address);
    
    // Step 2: Deploy safe (if not already deployed)
    let safe_path = PathBuf::from("./safe.yaml");
    
    if !SafeModulePersistence::exists(&safe_path) {
        let network_config = NetworkConfig {
            channels_contract_address: address!("0x77C9414043d27fdC98A6A2d73fc77b9b383092a7"),
            node_stake_factory_address: address!("0x439f5457FF58CEE941F7d946CB919c52EA30cfB3"),
        };
        
        let deployment_config = SafeDeploymentConfig::new(
            U256::from(rand::random::<u64>()),
            U256::from(1000000000000000000u128),
            vec![node_address],
        );
        
        let result = SafeDeployer::deploy(provider, &deployment_config, &network_config).await?;
        
        println!("Safe deployed at: {:?}", result.safe_address);
        
        // Step 3: Save safe configuration
        let safe_module = edgli::hopr_lib::config::SafeModule {
            safe_address: result.safe_address.into(),
            module_address: result.module_address.into(),
            ..Default::default()
        };
        
        SafeModulePersistence::store(&safe_module, &safe_path).await?;
        println!("Safe configuration saved");
    } else {
        let safe_module = SafeModulePersistence::load(&safe_path).await?;
        println!("Using existing safe: {:?}", safe_module.safe_address);
    }
    
    Ok(())
}
```

## Error Handling

All functions return `Result` types with appropriate error enums:

```rust
use gnosis_vpn_lib::edge_contrib::{IdentityManager, SafeModulePersistence};
use std::path::PathBuf;

async fn with_error_handling() {
    let id_path = PathBuf::from("./identity.id");
    let password = "wrong-password".to_string();
    
    match IdentityManager::load_from_file(&id_path, password) {
        Ok(keys) => println!("Loaded successfully"),
        Err(gnosis_vpn_lib::edge_contrib::identity::IdentityError::NotFound(path)) => {
            println!("Identity file not found: {}", path);
        }
        Err(gnosis_vpn_lib::edge_contrib::identity::IdentityError::KeyPair(err)) => {
            println!("Invalid password or corrupted file: {}", err);
        }
        Err(e) => println!("Other error: {}", e),
    }
    
    let safe_path = PathBuf::from("./safe.yaml");
    match SafeModulePersistence::load(&safe_path).await {
        Ok(safe) => println!("Safe loaded: {:?}", safe.safe_address),
        Err(gnosis_vpn_lib::edge_contrib::safe_persistence::PersistenceError::NotFound(path)) => {
            println!("Safe file not found: {}", path);
        }
        Err(e) => println!("Other error: {}", e),
    }
}
```

## Testing

See the test modules in each file for additional examples of usage:
- `gnosis_vpn-lib/src/edge_contrib/identity.rs` - Identity tests
- `gnosis_vpn-lib/src/edge_contrib/safe_deployment.rs` - Deployment tests
- `gnosis_vpn-lib/src/edge_contrib/safe_persistence.rs` - Persistence tests
