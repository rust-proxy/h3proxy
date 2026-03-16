use anyhow::{anyhow, Result};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use h3_quinn::BidiStream;
use h3::server::RequestStream;
use http::{Request, Response};
use http_body_util::{BodyExt, StreamBody};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tracing::{error, info};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;

pub async fn handle_request(
    req: Request<()>,
    mut stream: RequestStream<BidiStream<Bytes>, Bytes>,
    hyper_client: Client<HttpConnector, StreamBody<tokio_stream::wrappers::ReceiverStream<Result<hyper::body::Frame<Bytes>, anyhow::Error>>>>,
) -> Result<()> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let is_connect_udp = req.extensions().get::<h3::ext::Protocol>().map(|p| p.as_str() == "connect-udp").unwrap_or(false)
        || req.headers().get("protocol").map_or(false, |v| v == "connect-udp");

    info!(method = %method, uri = %uri, "incoming request");

    if method == http::Method::CONNECT {
        if is_connect_udp || uri.path().contains("/.well-known/masque/udp/") {
            return handle_connect_udp(req, stream).await;
        }

        let authority = req
            .headers()
            .get("host")
            .and_then(|v| v.to_str().ok())
            .or_else(|| uri.authority().map(|a| a.as_str()))
            .unwrap_or("");

        if authority.is_empty() {
            let resp = Response::builder().status(400).body(())?;
            stream.send_response(resp).await?;
            return Ok(());
        }

        info!(target = authority, "CONNECT -> tunneling");

        let mut parts = authority.splitn(2, ':');
        let host = parts.next().unwrap_or("");
        let port = parts.next().unwrap_or("443");
        let target = format!("{}:{}", host, port);

        match TcpStream::connect(target).await {
            Ok(tcp) => {
                let resp = Response::builder().status(200).body(())?;
                stream.send_response(resp).await?;

                let (mut tx, mut rx) = stream.split();
                let (mut tcp_read, mut tcp_write) = tcp.into_split();
                
                let to_tcp = tokio::spawn(async move {
                    while let Ok(Some(mut chunk)) = rx.recv_data().await {
                        let bytes = chunk.copy_to_bytes(chunk.remaining());
                        if let Err(e) = tcp_write.write_all(&bytes).await {
                            error!("tcp write err: {:?}", e);
                            break;
                        }
                    }
                    let _ = tcp_write.shutdown().await;
                });

                let to_client = tokio::spawn(async move {
                    let mut buf = [0u8; 8192];
                    loop {
                        match tcp_read.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                if let Err(e) = tx.send_data(Bytes::copy_from_slice(&buf[..n])).await {
                                    error!("send_data err: {:?}", e);
                                    break;
                                }
                            }
                            Err(e) => {
                                error!("tcp read err: {:?}", e);
                                break;
                            }
                        }
                    }
                    let _ = tx.finish().await;
                });

                let _ = tokio::try_join!(to_tcp, to_client);
            }
            Err(e) => {
                error!("connect target err: {:?}", e);
                let resp = Response::builder().status(502).body(())?;
                stream.send_response(resp).await?;
            }
        }
        return Ok(());
    }

    // ========== STREAM-OPTIMIZED FORWARDING (NON-CONNECT) ==========
// ... (rest is same)
    let upstream = if uri.scheme().is_some() && uri.host().is_some() {
        uri
    } else {
        let authority = req
            .headers()
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
        format!("http://{}{}", authority, path).parse()?
    };

    let mut builder = Request::builder().method(method).uri(upstream.clone());
    for (name, value) in req.headers().iter() {
        if name.as_str().eq_ignore_ascii_case("content-length") {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_bytes());
    }

    let (tx_body, rx_body) = tokio::sync::mpsc::channel::<Result<hyper::body::Frame<Bytes>, anyhow::Error>>(10);
    let stream_body = StreamBody::new(tokio_stream::wrappers::ReceiverStream::new(rx_body));
    let hyper_req = builder.body(stream_body)?;

    let (mut tx, mut rx) = stream.split();

    tokio::spawn(async move {
        while let Ok(Some(mut chunk)) = rx.recv_data().await {
            let bytes = chunk.copy_to_bytes(chunk.remaining());
            if let Err(_) = tx_body.send(Ok(hyper::body::Frame::data(bytes))).await {
                error!("failed to stream body to upstream");
                break;
            }
        }
    });

    let mut resp = hyper_client.request(hyper_req).await?;

    let mut resp_builder = http::response::Builder::new();
    resp_builder = resp_builder.status(resp.status().as_u16());
    for (name, value) in resp.headers().iter() {
        resp_builder = resp_builder.header(name.as_str(), value.as_bytes());
    }

    tx.send_response(resp_builder.body(())?).await?;
    
    while let Some(frame_res) = resp.body_mut().frame().await {
        if let Ok(frame) = frame_res {
            if let Some(chunk) = frame.data_ref() {
                tx.send_data(chunk.clone()).await?;
            }
        }
    }
    tx.finish().await?;

    Ok(())
}

async fn handle_connect_udp(
    req: Request<()>,
    mut stream: RequestStream<BidiStream<Bytes>, Bytes>,
) -> Result<()> {
    let uri = req.uri();
    
    // Parse target from path e.g., /.well-known/masque/udp/192.168.1.1/53/
    let path = uri.path();
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 6 {
        let resp = Response::builder().status(400).body(())?;
        stream.send_response(resp).await?;
        return Ok(());
    }
    
    // /.well-known/masque/udp/{host}/{port}/
    let host = parts[4];
    let port = parts[5];
    let target = format!("{}:{}", host, port);
    
    info!(target = target, "CONNECT-UDP -> UDP tunneling");

    let udp_socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(e) => {
            error!("udp bind err: {:?}", e);
            let resp = Response::builder().status(500).body(())?;
            stream.send_response(resp).await?;
            return Ok(());
        }
    };

    if let Err(e) = udp_socket.connect(&target).await {
        error!("udp connect err: {:?}", e);
        let resp = Response::builder().status(502).body(())?;
        stream.send_response(resp).await?;
        return Ok(());
    }

    let resp = Response::builder()
        .status(200)
        // .header("capsule-protocol", "?1") // indicates support for capsules
        .body(())?;
    stream.send_response(resp).await?;

    let (mut tx, mut rx) = stream.split();
    let udp = std::sync::Arc::new(udp_socket);
    let udp_recv = udp.clone();
    let udp_send = udp.clone();

    // Context context ID for Datagrams (simplified as 0 for RFC 9298)
    // Here we implement the Capsule framing over Stream fallback:
    // Capsule: Type (varint), Length (varint), Value (bytes)

    let to_udp = tokio::spawn(async move {
        while let Ok(Some(mut chunk)) = rx.recv_data().await {
            let bytes = chunk.copy_to_bytes(chunk.remaining());
            // In a real implementation, we would parse Capsule VarInts here.
            // For simple demonstration without strict Capsule parsing loop, 
            // we write raw chunks to UDP (not fully RFC 9298 compliant without Capsule parser).
            // Proper RFC 9298 Capsule parsing:
            // Type = 0x00 (DATAGRAM)
            // Length = N
            // Context ID = 0
            // UDP Payload
            if let Err(e) = udp_send.send(&bytes).await {
                error!("udp send err: {:?}", e);
                break;
            }
        }
    });

    let to_client = tokio::spawn(async move {
        let mut buf = [0u8; 65535];
        loop {
            match udp_recv.recv(&mut buf).await {
                Ok(n) => {
                    // Send as raw bytes or wrap in Capsule framing
                    // To be fully RFC 9298 compliant over stream, wrap in Capsule:
                    // [Type=0][Length=n+1][ContextID=0][Payload]
                    if let Err(e) = tx.send_data(Bytes::copy_from_slice(&buf[..n])).await {
                        error!("h3 send_data err: {:?}", e);
                        break;
                    }
                }
                Err(e) => {
                    error!("udp recv err: {:?}", e);
                    break;
                }
            }
        }
        let _ = tx.finish().await;
    });

    let _ = tokio::try_join!(to_udp, to_client);
    Ok(())
}
