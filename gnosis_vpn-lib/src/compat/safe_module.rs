use edgli::SafeModuleDeploymentResult;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct SafeModule {
    pub safe_address: String,
    pub module_address: String,
}

impl From<SafeModuleDeploymentResult> for SafeModule {
    fn from(result: SafeModuleDeploymentResult) -> Self {
        SafeModule {
            safe_address: result.safe_address.to_string(),
            module_address: result.module_address.to_string(),
        }
    }
}
