use anyhow::Result;
use bytes::{Buf, Bytes};
use h3_quinn::BidiStream;
use h3::server::RequestStream;
use http::{Request, Response};
use http_body_util::{BodyExt, StreamBody};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
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
    info!(method = %method, uri = %uri, "incoming request");

    if method == http::Method::CONNECT {
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
