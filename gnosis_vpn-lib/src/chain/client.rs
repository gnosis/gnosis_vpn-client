use alloy::{
    network::EthereumWallet,
    providers::{
        Identity, Provider, ProviderBuilder, RootProvider,
        fillers::{BlobGasFiller, ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller, WalletFiller},
    },
    signers::local::PrivateKeySigner,
};
use edgli::hopr_lib::{Keypair, prelude::ChainKeypair};
use reqwest::Url;

use crate::chain::errors::ChainError;

/// Default Gnosis chain RPC URL
pub const DEFAULT_GNOSIS_RPC_URL: &str = "https://gnosis.publicnode.com/";

pub(crate) type GnosisProvider = FillProvider<
    JoinFill<
        JoinFill<Identity, JoinFill<GasFiller, JoinFill<BlobGasFiller, JoinFill<NonceFiller, ChainIdFiller>>>>,
        WalletFiller<EthereumWallet>,
    >,
    RootProvider,
>;

/// RPC client for interacting with Gnosis chain
#[derive(Debug)]
pub struct GnosisRpcClient {
    pub provider: GnosisProvider,
    pub rpc_url: String,
}

impl GnosisRpcClient {
    /// Create a new RPC client with the default Gnosis chain RPC URL
    pub async fn new(private_key: ChainKeypair) -> Result<Self, ChainError> {
        Self::with_url(private_key, DEFAULT_GNOSIS_RPC_URL).await
    }

    /// Create a new RPC client with a custom RPC URL
    pub async fn with_url(private_key: ChainKeypair, rpc_url: &str) -> Result<Self, ChainError> {
        let signer = PrivateKeySigner::from_slice(private_key.secret().as_ref())?;
        let url = Url::parse(rpc_url)?;

        // Use the recommended method instead of deprecated on_http
        let provider = ProviderBuilder::new().wallet(signer).connect(url.as_str()).await?;

        Ok(Self {
            provider,
            rpc_url: rpc_url.to_string(),
        })
    }

    /// Get the RPC URL being used by this client
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    /// Get the current block number
    pub async fn get_block_number(&self) -> Result<u64, ChainError> {
        self.provider
            .get_block_number()
            .await
            .map_err(ChainError::AlloyRpcError)
    }

    /// Get the chain ID
    pub async fn get_chain_id(&self) -> Result<u64, ChainError> {
        self.provider.get_chain_id().await.map_err(ChainError::AlloyRpcError)
    }

    /// Check if the client is connected by attempting to get the chain ID
    pub async fn is_connected(&self) -> bool {
        self.get_chain_id().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::node_bindings::Anvil;
    use anyhow::Ok;

    #[tokio::test]
    async fn test_client_creation_with_default_url() -> anyhow::Result<()> {
        let default_signer = ChainKeypair::random();
        let client = GnosisRpcClient::new(default_signer).await;
        assert!(client.is_ok());

        let client = client?;
        assert_eq!(client.rpc_url(), DEFAULT_GNOSIS_RPC_URL);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Anvil to be running"]
    async fn test_client_creation_with_custom_url() -> anyhow::Result<()> {
        let anvil = Anvil::new().spawn();
        let default_signer = ChainKeypair::random();
        let custom_url = anvil.endpoint();
        let client = GnosisRpcClient::with_url(default_signer, custom_url.as_str()).await;
        assert!(client.is_ok());

        let client = client?;
        assert_eq!(client.rpc_url(), custom_url);

        let chain_id = client.get_chain_id().await;

        // Gnosis chain ID is 31337
        assert!(chain_id.is_ok());
        assert_eq!(chain_id?, 31337);
        Ok(())
    }
}
