use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;

pub struct ProxyConfig {
    pub listen_addr: SocketAddr,
    pub cert_chain: Vec<CertificateDer<'static>>,
    pub priv_key: PrivateKeyDer<'static>,
}
