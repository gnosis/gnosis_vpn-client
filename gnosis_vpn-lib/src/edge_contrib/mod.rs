//! Edge Client Contribution Module
//!
//! This module contains functionality that is intended to be contributed to the
//! hoprnet/edge-client repository. It includes:
//! - Safe module deployment logic
//! - Safe module persistence logic
//! - Identity file management utilities
//!
//! Once this functionality is available in edge-client, this module can be removed
//! and replaced with imports from the edgli crate.

pub mod safe_deployment;
pub mod safe_persistence;
pub mod identity;

pub use safe_deployment::{SafeDeployer, SafeDeploymentConfig, SafeDeploymentResult, NetworkConfig};
pub use safe_persistence::SafeModulePersistence;
pub use identity::{IdentityManager, IdentityConfig};
