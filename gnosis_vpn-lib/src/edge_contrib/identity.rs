//! Identity Management
//!
//! This module provides functionality for managing HOPR node identities.
//! This code is intended to be contributed to the hoprnet/edge-client repository.

use edgli::hopr_lib::exports::crypto::keypair::errors::KeyPairError;
use edgli::hopr_lib::{HoprKeys, IdentityRetrievalModes};
use rand::Rng;
use rand::distr::Alphanumeric;
use thiserror::Error;

use std::path::{Path, PathBuf};

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("Hopr key pair error: {0}")]
    KeyPair(#[from] KeyPairError),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Identity file not found: {0}")]
    NotFound(String),
}

/// Configuration for identity management
#[derive(Clone, Debug)]
pub struct IdentityConfig {
    /// Path to the identity file
    pub file_path: PathBuf,
    /// Password for the identity file
    pub password: String,
}

impl IdentityConfig {
    pub fn new(file_path: PathBuf, password: String) -> Self {
        Self {
            file_path,
            password,
        }
    }
}

/// Identity manager for HOPR nodes
pub struct IdentityManager;

impl IdentityManager {
    /// Load identity from a file
    ///
    /// # Arguments
    /// * `file` - Path to the identity file
    /// * `password` - Password to decrypt the identity
    ///
    /// # Returns
    /// The loaded HOPR keys or an error
    pub fn load_from_file(file: &Path, password: String) -> Result<HoprKeys, IdentityError> {
        if !file.exists() {
            return Err(IdentityError::NotFound(file.display().to_string()));
        }

        let id_path_owned = file.to_string_lossy().into_owned();
        let retrieval_mode = IdentityRetrievalModes::FromFile {
            password: password.as_str(),
            id_path: id_path_owned.as_str(),
        };
        HoprKeys::try_from(retrieval_mode).map_err(IdentityError::KeyPair)
    }

    /// Load identity from configuration
    ///
    /// # Arguments
    /// * `config` - The identity configuration
    ///
    /// # Returns
    /// The loaded HOPR keys or an error
    pub fn load_from_config(config: &IdentityConfig) -> Result<HoprKeys, IdentityError> {
        Self::load_from_file(&config.file_path, config.password.clone())
    }

    /// Generate a random password
    ///
    /// Generates a 48-character alphanumeric password suitable for protecting
    /// identity files.
    ///
    /// # Returns
    /// A randomly generated password string
    pub fn generate_password() -> String {
        rand::rng()
            .sample_iter(&Alphanumeric)
            .take(48)
            .map(char::from)
            .collect()
    }

    /// Create a new identity with a random password
    ///
    /// # Arguments
    /// * `file_path` - Path where the identity file should be created
    ///
    /// # Returns
    /// A tuple of (HoprKeys, password) or an error
    pub fn create_new(file_path: &Path) -> Result<(HoprKeys, String), IdentityError> {
        let password = Self::generate_password();
        let id_path_owned = file_path.to_string_lossy().into_owned();
        
        let retrieval_mode = IdentityRetrievalModes::FromFile {
            password: password.as_str(),
            id_path: id_path_owned.as_str(),
        };
        
        let keys = HoprKeys::try_from(retrieval_mode).map_err(IdentityError::KeyPair)?;
        
        Ok((keys, password))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
    use tempfile::TempDir;

    #[test]
    fn test_generate_password() {
        let password = IdentityManager::generate_password();
        
        // Password should be 48 characters
        assert_eq!(password.len(), 48);
        
        // Password should only contain alphanumeric characters
        assert!(password.chars().all(|c| c.is_ascii_alphanumeric()));
        
        // Generate another password and ensure they're different
        let password2 = IdentityManager::generate_password();
        assert_ne!(password, password2);
    }

    #[test]
    fn test_identity_config_new() {
        let path = PathBuf::from("/tmp/test.id");
        let password = "test-password".to_string();
        
        let config = IdentityConfig::new(path.clone(), password.clone());
        
        assert_eq!(config.file_path, path);
        assert_eq!(config.password, password);
    }

    #[test]
    fn test_load_from_file_not_found() {
        let path = PathBuf::from("/nonexistent/file.id");
        let password = "test-password".to_string();
        
        let result = IdentityManager::load_from_file(&path, password);
        
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), IdentityError::NotFound(_)));
    }

    #[test]
    fn test_create_new_identity() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let id_path = temp_dir.path().join("test.id");
        
        let result = IdentityManager::create_new(&id_path);
        
        // Creation should succeed
        assert!(result.is_ok());
        
        let (keys, password) = result.unwrap();
        
        // Password should be 48 characters
        assert_eq!(password.len(), 48);
        
        // Keys should be valid
        assert!(!keys.chain_key.public().to_address().is_zero());
        
        // Identity file should exist
        assert!(id_path.exists());
    }

    #[test]
    fn test_load_from_config() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let id_path = temp_dir.path().join("test.id");
        
        // Create a new identity
        let (_, password) = IdentityManager::create_new(&id_path).expect("Failed to create identity");
        
        // Create config and load identity
        let config = IdentityConfig::new(id_path, password);
        let result = IdentityManager::load_from_config(&config);
        
        assert!(result.is_ok());
    }

    #[test]
    fn test_roundtrip_create_and_load() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let id_path = temp_dir.path().join("test.id");
        
        // Create a new identity
        let (original_keys, password) = IdentityManager::create_new(&id_path)
            .expect("Failed to create identity");
        
        let original_address = original_keys.chain_key.public().to_address();
        
        // Load the identity back
        let loaded_keys = IdentityManager::load_from_file(&id_path, password)
            .expect("Failed to load identity");
        
        let loaded_address = loaded_keys.chain_key.public().to_address();
        
        // Addresses should match
        assert_eq!(original_address, loaded_address);
    }
}
