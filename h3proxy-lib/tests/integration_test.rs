use anyhow::Result;
use h3proxy_lib::client::{ClientConfig, ProxyClient};
use h3proxy_lib::config::ProxyConfig;
use h3proxy_lib::server::ProxyServer;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::time::{sleep, Duration};

/// Helper to generate a self-signed cert for tests in-memory using `rcgen`.
fn generate_test_cert() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    let cert = rcgen::generate_simple_self_signed(subject_alt_names)?;
    let cert_der = cert.serialize_der()?;
    let priv_key_der = cert.serialize_private_key_der();

    let cert_chain = vec![CertificateDer::from(cert_der)];
    let priv_key = PrivateKeyDer::Pkcs8(priv_key_der.into());

    Ok((cert_chain, priv_key))
}

#[tokio::test]
async fn test_proxy_server_and_client_connect() -> Result<()> {
    // 1. Generate test TLS certificates
    let (cert_chain, priv_key) = generate_test_cert()?;

    // 2. Start a dummy local TCP echo server (simulate target website)
    let target_listener = TcpListener::bind("127.0.0.1:0").await?;
    let target_addr = target_listener.local_addr()?;
    
    tokio::spawn(async move {
        if let Ok((mut socket, _)) = target_listener.accept().await {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 1024];
            if let Ok(n) = socket.read(&mut buf).await {
                // Echo what was received for simplicity
                let _ = socket.write_all(&buf[..n]).await;
            }
        }
    });

    // 3. Start the h3proxy Server on a random port
    let proxy_addr: SocketAddr = "127.0.0.1:4433".parse()?; // Fixed port to simplify config

    let server_config = ProxyConfig {
        listen_addr: proxy_addr,
        cert_chain: cert_chain.clone(),
        priv_key,
    };
    
    let server = ProxyServer::new(server_config);
    
    tokio::spawn(async move {
        let _ = server.serve().await;
    });

    // Give server a moment to bind
    sleep(Duration::from_millis(500)).await;

    // 4. Run the Client and make a CONNECT request
    let client_config = ClientConfig {
        proxy_addr,
        target_url: format!("https://{}", target_addr),
        root_certs: cert_chain,
    };

    let client = ProxyClient::new(client_config);
    
    // We expect the run function to succeed and print out the echo response.
    // If the tunnel works, the `run` method completes `Ok(())`
    let result = client.run().await;
    assert!(result.is_ok(), "Client run failed: {:?}", result.err());

    Ok(())
}
