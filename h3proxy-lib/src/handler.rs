use anyhow::Result;
use bytes::Bytes;
use h3::quic::BidiStream;
use h3::server::RequestStream;
use http::{Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{error, info};

pub async fn handle_request(
    req: h3::Request<()>,
    stream: &mut RequestStream<BidiStream<quinn::Connection>>,
    hyper_client: hyper::Client<hyper::client::HttpConnector>,
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
            let _ = stream.send_response(resp).await?;
            return Ok(());
        }

        info!(target = authority, "CONNECT -> tunneling");

        let mut parts = authority.splitn(2, ':');
        let host = parts.next().unwrap_or("");
        let port = parts.next().unwrap_or("443");
        let target = format!("{}:{}", host, port);

        match TcpStream::connect(target).await {
            Ok(mut tcp) => {
                let resp = Response::builder().status(200).body(())?;
                let mut send = stream.send_response(resp).await?;

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
                    let _ = tcp_write.shutdown().await;
                });

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
                            Err(e) => {
                                error!("tcp read err: {:?}", e);
                                break;
                            }
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
        if let Some(name) = name {
            if name.as_str().eq_ignore_ascii_case("content-length") {
                continue;
            }
            builder = builder.header(name.as_str(), value.as_bytes());
        }
    }

    // 创建 hyper 请求体 channel 从而实现边读取 h3 stream 边向 hyper 输入数据
    let (mut sender, body) = hyper::Body::channel();
    let hyper_req = builder.body(body)?;

    // 开启单独的协程，将 HTTP/3 客户端传来的 body 流式转发给 hyper 上游请求
    let mut req_stream = stream.clone();
    tokio::spawn(async move {
        while let Some(chunk_res) = req_stream.data().await.unwrap_or(None) {
            match chunk_res {
                Ok(chunk) => {
                    let bytes = Bytes::copy_from_slice(&chunk);
                    if let Err(e) = sender.send_data(bytes).await {
                        error!("failed to stream body to upstream: {:?}", e);
                        break;
                    }
                }
                Err(e) => {
                    error!("error reading from h3 stream: {:?}", e);
                    break;
                }
            }
        }
    });

    // 向上游发起请求并等待响应头部
    let resp = hyper_client.request(hyper_req).await?;

    // 构建回传客户端的 Response
    let mut resp_builder = http::response::Builder::new();
    resp_builder = resp_builder.status(resp.status().as_u16());
    for (name, value) in resp.headers().iter() {
        resp_builder = resp_builder.header(name.as_str(), value.as_bytes());
    }

    // 返回响应头部给客户端
    let mut send = stream.send_response(resp_builder.body(())?).await?;
    
    // 流式读取上游 HTTP/1.1 返回的 body 并逐块写入到 HTTP/3 的下行流中
    let mut body_stream = resp.into_body();
    while let Some(chunk) = body_stream.data().await {
        let data = chunk?;
        send.send_data(Bytes::copy_from_slice(&data)).await?;
    }
    send.finish().await?;

    Ok(())
}
