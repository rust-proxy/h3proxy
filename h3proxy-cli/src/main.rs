use anyhow::{Context, Result};
use clap::Parser;
use h3proxy_lib::config::ProxyConfig;
use h3proxy_lib::server::ProxyServer;
use std::fs;
use std::net::SocketAddr;
use tracing_subscriber;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "0.0.0.0:4433")]
    listen: SocketAddr,

    #[arg(short, long, default_value = "cert.pem")]
    cert: String,

    #[arg(short, long, default_value = "key.pem")]
    key: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let cert_data = fs::read(&args.cert).with_context(|| format!("failed to read cert file: {}", args.cert))?;
    let key_data = fs::read(&args.key).with_context(|| format!("failed to read key file: {}", args.key))?;

    let cert_chain = quinn::CertificateChain::from_pem(&cert_data)?;
    let priv_key = quinn::PrivateKey::from_pem(&key_data)?;

    let config = ProxyConfig {
        listen_addr: args.listen,
        cert_chain,
        priv_key,
    };

    let server = ProxyServer::new(config);
    server.serve().await?;

    Ok(())
}
