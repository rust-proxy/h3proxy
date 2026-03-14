use anyhow::Result;
use h3_quinn::Connection;
use quinn::Endpoint;
use std::sync::Arc;
use tracing::{error, info};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

use crate::config::ProxyConfig;
use crate::handler::handle_request;

pub struct ProxyServer {
    pub config: ProxyConfig,
}

impl ProxyServer {
    pub fn new(config: ProxyConfig) -> Self {
        Self { config }
    }

    pub async fn serve(self) -> Result<()> {
        let mut server_crypto = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(self.config.cert_chain, self.config.priv_key)?;
        server_crypto.alpn_protocols = vec![b"h3".to_vec(), b"h3-29".to_vec()];

        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?));
        server_config.transport_config(Arc::new(quinn::TransportConfig::default()));

        let addr = self.config.listen_addr;
        let endpoint = Endpoint::server(server_config, addr)?;
        info!(%addr, "h3 proxy listening");

        let hyper_client = Client::builder(TokioExecutor::new()).build_http();

        while let Some(connecting) = endpoint.accept().await {
            let quinn_conn = match connecting.await {
                Ok(c) => c,
                Err(e) => {
                    error!("accept conn err: {:?}", e);
                    continue;
                }
            };

            let hyper_client = hyper_client.clone();
            tokio::spawn(async move {
                info!(peer = %quinn_conn.remote_address(), "accepted connection");
                let h3_conn = Connection::new(quinn_conn);
                let mut h3_server = match h3::server::builder()
                    .build(h3_conn)
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        error!("h3 build err: {:?}", e);
                        return;
                    }
                };

                while let Ok(Some(req_resolver)) = h3_server.accept().await {
                    if let Ok(resolved) = req_resolver.resolve_request().await {
                        let (req, stream) = resolved;
                        let hyper_client = hyper_client.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_request(req, stream, hyper_client).await {
                                error!("request err: {:?}", e);
                            }
                        });
                    }
                }
                info!("connection closed");
            });
        }

        Ok(())
    }
}
