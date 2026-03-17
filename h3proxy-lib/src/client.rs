use anyhow::Result;
use bytes::Buf;
use h3_quinn::Connection;
use http::Request;
use quinn::Endpoint;
use rustls_pki_types::CertificateDer;
use std::{net::SocketAddr, sync::Arc};
use tracing::info;

pub struct ClientConfig {
    pub proxy_addr: SocketAddr,
    pub target_url: String,
    pub root_certs: Vec<CertificateDer<'static>>,
}

pub struct ProxyClient {
    config: ClientConfig,
}

impl ProxyClient {
    pub fn new(config: ClientConfig) -> Self {
        Self { config }
    }

    pub async fn run(&self) -> Result<()> {
        let bind_addr: SocketAddr = if self.config.proxy_addr.is_ipv4() {
            "0.0.0.0:0".parse()?
        } else {
            "[::]:0".parse()?
        };
        let mut endpoint = Endpoint::client(bind_addr)?;

        let mut roots = rustls::RootCertStore::empty();
        for c in self.config.root_certs.clone() {
            roots.add(c)?;
        }

        let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        crypto.alpn_protocols = vec![b"h3".to_vec(), b"h3-29".to_vec()];

        let client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?
        ));
        endpoint.set_default_client_config(client_config);

        let quinn_conn = endpoint.connect(self.config.proxy_addr, "localhost")?.await?;
        info!("connected to proxy at {}", self.config.proxy_addr);

        let h3_conn_wrapped = Connection::new(quinn_conn);
        let (mut driver, mut send_request) = h3::client::new(h3_conn_wrapped).await?;
        tokio::spawn(async move {
            let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
        });

        let uri = self.config.target_url.parse::<http::Uri>()?;
        let host = uri.host().unwrap_or("");
        let port = uri.port_u16().unwrap_or(443);
        let authority = format!("{}:{}", host, port);

        let req = Request::builder()
            .method("CONNECT")
            .uri(self.config.target_url.clone())
            .header("host", authority)
            .body(())?;

        info!("sending CONNECT request to {}", self.config.target_url);
        let mut request_stream = send_request.send_request(req).await?;
        let resp = request_stream.recv_response().await?;
        info!("response status: {}", resp.status());

        while let Some(chunk) = request_stream.recv_data().await? {
            let bytes = chunk;
            print!("{}", String::from_utf8_lossy(bytes.chunk()));
        }

        Ok(())
    }
}
