use rustls::{Certificate, PrivateKey};
use std::net::SocketAddr;

pub struct ProxyConfig {
    pub listen_addr: SocketAddr,
    pub cert_chain: Vec<Certificate>,
    pub priv_key: PrivateKey,
}
