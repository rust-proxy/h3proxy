use anyhow::Result;
use futures::TryStreamExt;
use h3::quic::BidiStream;
use quinn::Endpoint;
use std::sync::Arc;
use tracing::{error, info};

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
        let mut server_config =
            quinn::ServerConfig::with_single_cert(self.config.cert_chain, self.config.priv_key)?;
        server_config.transport = Some(Arc::new(quinn::TransportConfig::default()));

        let addr = self.config.listen_addr;
        let (_endpoint, mut incoming) = Endpoint::server(server_config, addr)?;
        info!(%addr, "h3 proxy listening");

        let hyper_client = hyper::Client::builder().build_http();

        while let Some(connecting) = incoming.next().await {
            let connection = match connecting.await {
                Ok(c) => c,
                Err(e) => {
                    error!("accept conn err: {:?}", e);
                    continue;
                }
            };

            let hyper_client = hyper_client.clone();
            tokio::spawn(async move {
                info!(peer = %connection.remote_address(), "accepted connection");
                let mut h3_conn = match h3::server::builder()
                    .build(BidiStream::new(connection))
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        error!("h3 build err: {:?}", e);
                        return;
                    }
                };

                while let Some(accept) = h3_conn.accept().await.unwrap_or(None) {
                    let (req, mut stream) = accept;
                    let hyper_client = hyper_client.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_request(req, &mut stream, hyper_client).await {
                            error!("request err: {:?}", e);
                        }
                    });
                }
                info!("connection closed");
            });
        }

        Ok(())
    }
}
