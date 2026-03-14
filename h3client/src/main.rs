use anyhow::Result;
use quinn::Endpoint;
use std::{fs, net::SocketAddr, sync::Arc};
use http::Request;
use h3_quinn::Connection;
use bytes::Buf;
use rustls::pki_types::CertificateDer;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let server_addr: SocketAddr = "127.0.0.1:4433".parse()?;
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;

    // trust self-signed cert
    let cert = fs::read("cert.pem")?;
    let mut roots = rustls::RootCertStore::empty();
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &*cert).collect::<Result<_, _>>()?;
    for c in certs {
        roots.add(c)?;
    }
    
    let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![b"h3".to_vec(), b"h3-29".to_vec()];

    let client_config = quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?));
    endpoint.set_default_client_config(client_config);

    let quinn_conn = endpoint.connect(server_addr, "localhost")?.await?;
    println!("connected");

    let h3_conn_wrapped = Connection::new(quinn_conn);
    let (mut driver, mut send_request) = h3::client::new(h3_conn_wrapped).await?;
    tokio::spawn(async move {
        let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
    });

    // Example: send CONNECT to create tunnel to example.com:443
    let req = Request::builder()
        .method("CONNECT")
        .uri("https://example.com:443")
        .header("host", "example.com:443")
        .body(())?;

    let mut request_stream = send_request.send_request(req).await?;
    let resp = request_stream.recv_response().await?;
    println!("resp status: {}", resp.status());

    while let Some(chunk) = request_stream.recv_data().await? {
        let bytes = chunk;
        print!("{}", String::from_utf8_lossy(bytes.chunk()));
    }

    Ok(())
}
