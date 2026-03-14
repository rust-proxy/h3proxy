use anyhow::Result;
use bytes::Bytes;
use futures::TryStreamExt;
use h3::server::{RequestStream, Server};
use h3::quic::BidiStream;
use quinn::{Endpoint};
use std::{fs, net::SocketAddr, sync::Arc};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpStream};
use tracing::{info, error};
use tracing_subscriber;
use http::{Request, Response};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let addr: SocketAddr = "0.0.0.0:4433".parse()?;

    let cert = fs::read("cert.pem")?;
    let key = fs::read("key.pem")?;
    let cert_chain = quinn::CertificateChain::from_pem(&cert)?;
    let priv_key = quinn::PrivateKey::from_pem(&key)?;

    let mut server_config = quinn::ServerConfig::with_single_cert(cert_chain, priv_key)?;
    server_config.transport = Some(Arc::new(quinn::TransportConfig::default()));

    let (_endpoint, mut incoming) = Endpoint::server(server_config, addr)?;
    info!(%addr, "h3 proxy listening");

    // simple hyper client for non-CONNECT forwarding
    let hyper_client = hyper::Client::new();

    while let Some(connecting) = incoming.next().await {
        let connection = match connecting.await {
            Ok(c) => c,
            Err(e) => { error!("accept conn err: {:?}", e); continue; }
        };
        let hyper_client = hyper_client.clone();
        tokio::spawn(async move {
            info!(peer = %connection.remote_address(), "accepted connection");
            // build h3 server on top of quinn connection
            let mut h3_conn = match h3::server::builder().build(BidiStream::new(connection)).await {
                Ok(c) => c,
                Err(e) => { error!("h3 build err: {:?}", e); return; }
            };

            while let Some(accept) = h3_conn.accept().await.unwrap_or(None) {
                let (req, mut stream) = accept;
                let hyper_client = hyper_client.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(req, &mut stream, hyper_client).await {
                        error!("request err: {:?}", e);
                    }
                });
            }
            info!("connection closed");
        });
    }

    Ok(())
}

async fn handle(
    req: h3::Request<()>,
    stream: &mut RequestStream<BidiStream<quinn::Connection>>,
    hyper_client: hyper::Client<hyper::client::HttpConnector>,
) -> Result<()> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    info!(method = %method, uri = %uri, "incoming request");

    if method == http::Method::CONNECT {
        // Expect :authority like "example.com:443"
        let authority = req
            .headers()
            .get("host")
            .and_then(|v| v.to_str().ok())
            .or_else(|| uri.authority().map(|a| a.as_str()))
            .unwrap_or("");

        if authority.is_empty() {
            let resp = Response::builder().status(400).body(())?;
            let _ = stream.send_response(resp).await?;
            return Ok(());
        }

        info!(target = authority, "CONNECT -> tunneling");

        // open TCP to target
        let mut parts = authority.splitn(2, ':');
        let host = parts.next().unwrap_or("");
        let port = parts.next().unwrap_or("443");
        let target = format!("{}:{}", host, port);

        match TcpStream::connect(target).await {
            Ok(mut tcp) => {
                // reply 200
                let resp = Response::builder().status(200).body(())?;
                let mut send = stream.send_response(resp).await?;

                // spawn task: read from client (h3) and write to tcp
                let mut recv = stream.clone();
                let mut tcp_write = tcp.clone();
                let to_tcp = tokio::spawn(async move {
                    while let Some(chunk) = recv.data().await.unwrap_or(None) {
                        let data = chunk.unwrap_or_default();
                        if let Err(e) = tcp_write.write_all(&data).await {
                            error!("tcp write err: {:?}", e);
                            break;
                        }
                    }
                    // close tcp write
                    let _ = tcp_write.shutdown().await;
                });

                // read from tcp and send to client
                let to_client = tokio::spawn(async move {
                    let mut buf = [0u8; 8192];
                    loop {
                        match tcp.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                if let Err(e) = send.send_data(Bytes::copy_from_slice(&buf[..n])).await {
                                    error!("send_data err: {:?}", e);
                                    break;
                                }
                            }
                            Err(e) => { error!("tcp read err: {:?}", e); break; }
                        }
                    }
                    let _ = send.finish().await;
                });

                let _ = tokio::try_join!(to_tcp, to_client);
            }
            Err(e) => {
                error!("connect target err: {:?}", e);
                let resp = Response::builder().status(502).body(())?;
                let _ = stream.send_response(resp).await?;
            }
        }

        return Ok(());
    }

    // Non-CONNECT: simple proxy using hyper (issue simplified: only http scheme)
    let upstream = if uri.scheme().is_some() && uri.host().is_some() {
        uri
    } else {
        let authority = req
            .headers()
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
        format!("http://{}{}", authority, path).parse()?;
    };

    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(upstream.clone());
    for (name, value) in req.headers().iter() {
        if let Some(name) = name {
            if name.as_str().eq_ignore_ascii_case("content-length") { continue; }
            builder = builder.header(name.as_str(), value.as_bytes());
        }
    }

    // read body
    let mut body = Vec::new();
    while let Some(chunk) = stream.data().await.unwrap_or(None) {
        body.extend_from_slice(&chunk?);
    }

    let hyper_req = builder.body(hyper::Body::from(body))?;
    let resp = hyper_client.request(hyper_req).await?;

    let mut resp_builder = http::response::Builder::new();
    resp_builder = resp_builder.status(resp.status().as_u16());
    for (name, value) in resp.headers().iter() {
        resp_builder = resp_builder.header(name.as_str(), value.as_bytes());
    }

    let mut send = stream.send_response(resp_builder.body(())?).await?;
    let mut body_stream = resp.into_body();
    while let Some(chunk) = body_stream.data().await {
        let data = chunk?;
        send.send_data(Bytes::copy_from_slice(&data)).await?;
    }
    send.finish().await?;

    Ok(())
}
