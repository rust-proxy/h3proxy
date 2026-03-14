use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use h3proxy_lib::client::{ClientConfig, ProxyClient};
use h3proxy_lib::config::ProxyConfig;
use h3proxy_lib::server::ProxyServer;
use std::fs;
use std::net::SocketAddr;

#[derive(Parser, Debug)]
#[command(author, version, about = "h3proxy - A universal HTTP/3 proxy platform", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the proxy server (inbound)
    Server {
        #[arg(short, long, default_value = "0.0.0.0:4433")]
        listen: SocketAddr,

        #[arg(short, long, default_value = "cert.pem")]
        cert: String,

        #[arg(short, long, default_value = "key.pem")]
        key: String,
    },
    /// Run the proxy client (outbound test tunnel)
    Client {
        #[arg(short, long, default_value = "127.0.0.1:4433")]
        proxy: SocketAddr,

        #[arg(short, long, default_value = "https://example.com:443")]
        target: String,

        #[arg(short, long, default_value = "cert.pem")]
        cert: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::Server { listen, cert, key } => {
            let cert_data = fs::read(&cert).with_context(|| format!("failed to read cert file: {}", cert))?;
            let key_data = fs::read(&key).with_context(|| format!("failed to read key file: {}", key))?;

            let certs: Vec<_> = rustls_pemfile::certs(&mut &*cert_data)
                .collect::<Result<_, _>>()
                .context("failed to parse certs")?;

            let mut keys: Vec<_> = rustls_pemfile::private_key(&mut &*key_data)
                .context("failed to parse key")?
                .into_iter()
                .collect();
            let priv_key = keys.remove(0);

            let config = ProxyConfig {
                listen_addr: listen,
                cert_chain: certs,
                priv_key,
            };

            let server = ProxyServer::new(config);
            server.serve().await?;
        }
        Commands::Client { proxy, target, cert } => {
            let cert_data = fs::read(&cert).with_context(|| format!("failed to read cert file: {}", cert))?;
            let certs: Vec<_> = rustls_pemfile::certs(&mut &*cert_data)
                .collect::<Result<_, _>>()
                .context("failed to parse certs")?;

            let config = ClientConfig {
                proxy_addr: proxy,
                target_url: target,
                root_certs: certs,
            };

            let client = ProxyClient::new(config);
            client.run().await?;
        }
    }

    Ok(())
}
