use quinn::{CertificateChain, PrivateKey};
use std::net::SocketAddr;

pub struct ProxyConfig {
    pub listen_addr: SocketAddr,
    pub cert_chain: CertificateChain,
    pub priv_key: PrivateKey,
}
