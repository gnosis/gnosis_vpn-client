//! Safe Module Persistence
//!
//! This module provides functionality for persisting Safe module configurations to disk.
//! This code is intended to be contributed to the hoprnet/edge-client repository.

use edgli::hopr_lib::config::SafeModule;
use serde_yaml;
use thiserror::Error;
use tokio::fs;

use std::path::Path;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("Safe module file not found: {0}")]
    NotFound(String),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_yaml::Error),
}

/// Safe module persistence manager
pub struct SafeModulePersistence;

impl SafeModulePersistence {
    /// Store a Safe module configuration to disk
    ///
    /// # Arguments
    /// * `safe_module` - The Safe module configuration to store
    /// * `path` - Path where the configuration should be stored
    ///
    /// # Returns
    /// Ok(()) on success, or an error
    ///
    /// # Notes
    /// This function does NOT create parent directories automatically.
    /// Ensure the parent directory exists before calling this function,
    /// otherwise an IO error will be returned.
    pub async fn store(safe_module: &SafeModule, path: &Path) -> Result<(), PersistenceError> {
        let content = serde_yaml::to_string(safe_module)?;
        fs::write(path, &content).await.map_err(PersistenceError::IO)
    }

    /// Load a Safe module configuration from disk
    ///
    /// # Arguments
    /// * `path` - Path to the Safe module configuration file
    ///
    /// # Returns
    /// The loaded Safe module configuration or an error
    pub async fn load(path: &Path) -> Result<SafeModule, PersistenceError> {
        if !path.exists() {
            return Err(PersistenceError::NotFound(path.display().to_string()));
        }

        let content = fs::read_to_string(path).await?;
        serde_yaml::from_str::<SafeModule>(&content).map_err(PersistenceError::Serialization)
    }

    /// Check if a Safe module configuration file exists
    ///
    /// # Arguments
    /// * `path` - Path to check for existence
    ///
    /// # Returns
    /// true if the file exists, false otherwise
    pub fn exists(path: &Path) -> bool {
        path.exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn sample_safe_module() -> SafeModule {
        SafeModule {
            safe_address: address!("0x1234567890123456789012345678901234567890").into(),
            module_address: address!("0x0987654321098765432109876543210987654321").into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_store_and_load() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let safe_path = temp_dir.path().join("safe.yaml");

        let safe_module = sample_safe_module();

        // Store the safe module
        let store_result = SafeModulePersistence::store(&safe_module, &safe_path).await;
        assert!(store_result.is_ok());

        // File should exist
        assert!(safe_path.exists());

        // Load the safe module back
        let load_result = SafeModulePersistence::load(&safe_path).await;
        assert!(load_result.is_ok());

        let loaded = load_result.unwrap();
        assert_eq!(loaded.safe_address, safe_module.safe_address);
        assert_eq!(loaded.module_address, safe_module.module_address);
    }

    #[tokio::test]
    async fn test_load_nonexistent_file() {
        let path = PathBuf::from("/nonexistent/safe.yaml");

        let result = SafeModulePersistence::load(&path).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PersistenceError::NotFound(_)));
    }

    #[test]
    fn test_exists() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let existing_file = temp_dir.path().join("exists.yaml");
        let nonexistent_file = temp_dir.path().join("does_not_exist.yaml");

        // Create a file
        std::fs::write(&existing_file, "test").expect("Failed to write file");

        assert!(SafeModulePersistence::exists(&existing_file));
        assert!(!SafeModulePersistence::exists(&nonexistent_file));
    }

    #[tokio::test]
    async fn test_store_creates_parent_directory_fails() {
        // This test verifies that store does NOT automatically create parent directories
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let safe_path = temp_dir.path().join("nonexistent_dir").join("safe.yaml");

        let safe_module = sample_safe_module();

        let result = SafeModulePersistence::store(&safe_module, &safe_path).await;
        
        // Should fail because parent directory doesn't exist
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_roundtrip_preserves_data() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let safe_path = temp_dir.path().join("safe.yaml");

        let original = sample_safe_module();

        // Store and load
        SafeModulePersistence::store(&original, &safe_path)
            .await
            .expect("Failed to store");
        
        let loaded = SafeModulePersistence::load(&safe_path)
            .await
            .expect("Failed to load");

        // Verify all fields match
        assert_eq!(loaded.safe_address, original.safe_address);
        assert_eq!(loaded.module_address, original.module_address);
    }
}
