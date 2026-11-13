//! The TLS settings every connection uses: ring for crypto, the OS trust store for certificates.

use std::sync::Arc;

use rustls_platform_verifier::BuilderVerifierExt as _;
use tokio_rustls::rustls::ClientConfig;
use tokio_rustls::rustls::crypto::ring;

use crate::Result;

/// A client configuration that checks certificates against the operating system's trust store.
pub(crate) fn client_config() -> Result<ClientConfig> {
    Ok(
        ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_platform_verifier()?
            .with_no_client_auth(),
    )
}
