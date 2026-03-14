use anyhow::Result;
use quinn::Endpoint;
use std::{fs, net::SocketAddr, sync::Arc};
use tracing_subscriber;
use http::Request;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let server_addr: SocketAddr = "127.0.0.1:4433".parse()?;
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;

    // trust self-signed cert
    let cert = fs::read("cert.pem")?;
    let mut roots = rustls::RootCertStore::empty();
    let certs = rustls_pemfile::certs(&mut &*cert)?;
    for c in certs {
        roots.add(&rustls::Certificate(c))?;
    }
    let crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut client_config = quinn::ClientConfig::new(Arc::new(crypto));
    client_config.alpn_protocols = vec![b"h3".to_vec(), b"h3-29".to_vec()];
    endpoint.set_default_client_config(client_config);

    let conn = endpoint.connect(server_addr, "localhost")?.await?;
    println!("connected");

    let (mut h3_conn, driver) = h3::client::new(conn).await?;
    tokio::spawn(async move { let _ = driver.await; });

    // Example: send CONNECT to create tunnel to example.com:443
    let req = Request::builder()
        .method("CONNECT")
        .uri("https://example.com:443")
        .header("host", "example.com:443")
        .body(())?;

    let (resp, mut recv_body) = h3_conn.send_request(req).await?;
    println!("resp status: {}", resp.status());

    // If 200, you can start exchanging raw bytes on the stream; here we just print any returned data
    while let Some(chunk) = recv_body.data().await? {
        print!("{}", String::from_utf8_lossy(&chunk));
    }

    Ok(())
}
