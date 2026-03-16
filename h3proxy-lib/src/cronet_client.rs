use anyhow::Result;
use std::net::SocketAddr;
use rustls_pki_types::CertificateDer;

pub struct CronetClientConfig {
    pub proxy_addr: SocketAddr,
    pub target_url: String,
    pub root_certs: Vec<CertificateDer<'static>>,
}

pub struct CronetClient {
    config: CronetClientConfig,
}

impl CronetClient {
    pub fn new(config: CronetClientConfig) -> Self {
        Self { config }
    }

    pub async fn run(&self) -> Result<()> {
        #[cfg(feature = "cronet-backend")]
        {
            use cronet_rs::{
                Buffer, CronetError, Engine, EngineParams, Executor, Runnable, UrlRequest,
                UrlRequestCallback, UrlRequestCallbackHandler, UrlRequestParams, UrlResponseInfo,
            };
            use tokio::sync::{mpsc, oneshot};

            tracing::info!("cronet-rs backend is enabled, attempting to route through proxy: {}", self.config.proxy_addr);

            // 1. Setup Engine
            // TODO: 如果在 build 或 run 阶段遇到 libcronet 找不到的 C 链接错误，
            // 请手动下载 cronet 预编译库 (libcronet.so/.dylib/.dll)，并设置正确的
            // LD_LIBRARY_PATH 或将其放置在 target 的 deps 目录下。
            let engine_params = EngineParams::new();
            engine_params.set_enable_quic(true);
            engine_params.set_enable_http_2(true);

            // Proxy via our H3 server
            let proxy_rule = format!("QUIC://{}", self.config.proxy_addr);
            let experimental_options = format!(r#"{{"env_options": {{"proxy": "{}"}}}}"#, proxy_rule);
            engine_params.set_experimental_options(&experimental_options);

            let engine = Engine::new();
            let res = engine.start(engine_params);
            if res != cronet_rs::EngineResult::Success {
                anyhow::bail!("Failed to start cronet engine: {:?}", res);
            }

            // 2. Setup Executor to run on tokio blocking threads
            fn tokio_execute(_executor: Executor, runnable: Runnable) {
                tokio::task::spawn_blocking(move || {
                    use cronet_rs::Destroy;
                    runnable.run();
                    runnable.destroy();
                });
            }
            let executor = Executor::new(tokio_execute);

            // 3. Setup Callback
            let (tx, mut rx) = mpsc::channel::<Vec<u8>>(32);
            let (done_tx, done_rx) = oneshot::channel::<Result<()>>();

            struct CallbackHandler {
                tx: mpsc::Sender<Vec<u8>>,
                done_tx: Option<oneshot::Sender<Result<()>>>,
            }

            impl UrlRequestCallbackHandler for CallbackHandler {
                fn on_redirect_received(
                    &mut self,
                    _callback: UrlRequestCallback,
                    request: UrlRequest,
                    _info: UrlResponseInfo,
                    _new_location_url: &str,
                ) {
                    tracing::info!("cronet: redirect received");
                    let _ = request.follow_redirect();
                }

                fn on_response_started(
                    &mut self,
                    _callback: UrlRequestCallback,
                    request: UrlRequest,
                    info: UrlResponseInfo,
                ) {
                    tracing::info!("cronet: response started, status: {}", info.status_code());
                    let buffer = Buffer::new_with_size(8192);
                    let _ = request.read(buffer);
                }

                fn on_read_completed(
                    &mut self,
                    _callback: UrlRequestCallback,
                    request: UrlRequest,
                    _info: UrlResponseInfo,
                    buffer: Buffer,
                    bytes_read: u64,
                ) {
                    let data = buffer.data_slice::<u8>(bytes_read as usize);
                    let vec_data = data.to_vec();
                    
                    use cronet_rs::Destroy;
                    buffer.destroy();

                    // Send data to the reader task
                    if let Err(e) = self.tx.blocking_send(vec_data) {
                        tracing::error!("cronet: failed to send data to tokio task: {:?}", e);
                        request.cancel();
                        return;
                    }

                    // Request next chunk
                    // We can reuse the buffer or create a new one.
                    // For safety, we allocate a new one.
                    let next_buffer = Buffer::new_with_size(8192);
                    let _ = request.read(next_buffer);
                }

                fn on_succeeded(
                    &mut self,
                    _callback: UrlRequestCallback,
                    _request: UrlRequest,
                    _info: UrlResponseInfo,
                ) {
                    tracing::info!("cronet: request succeeded");
                    if let Some(tx) = self.done_tx.take() {
                        let _ = tx.send(Ok(()));
                    }
                }

                fn on_failed(
                    &mut self,
                    _callback: UrlRequestCallback,
                    _request: UrlRequest,
                    _info: UrlResponseInfo,
                    error: CronetError,
                ) {
                    tracing::error!("cronet: request failed: {:?}", error.error_code());
                    if let Some(tx) = self.done_tx.take() {
                        let _ = tx.send(Err(anyhow::anyhow!("cronet request failed with error code {:?}", error.error_code())));
                    }
                }

                fn on_canceled(
                    &mut self,
                    _callback: UrlRequestCallback,
                    _request: UrlRequest,
                    _info: UrlResponseInfo,
                ) {
                    tracing::warn!("cronet: request canceled");
                    if let Some(tx) = self.done_tx.take() {
                        let _ = tx.send(Err(anyhow::anyhow!("cronet request canceled")));
                    }
                }
            }

            // We must create a new object of handler
            let handler = CallbackHandler {
                tx,
                done_tx: Some(done_tx),
            };
            let callback = UrlRequestCallback::new(handler);

            // 4. Setup Request Params
            let params = UrlRequestParams::new();
            params.set_method("CONNECT");

            // 5. Initialize and Start Request
            let request = UrlRequest::new();
            let res = request.init_with_params(
                &engine,
                &self.config.target_url,
                &params,
                &callback,
                &executor,
            );
            if res != cronet_rs::EngineResult::Success {
                anyhow::bail!("Failed to init cronet request: {:?}", res);
            }

            let res = request.start();
            if res != cronet_rs::EngineResult::Success {
                anyhow::bail!("Failed to start cronet request: {:?}", res);
            }

            // 6. Handle incoming data
            let reader_task = tokio::spawn(async move {
                while let Some(chunk) = rx.recv().await {
                    print!("{}", String::from_utf8_lossy(&chunk));
                }
            });

            // wait for the request to complete
            done_rx.await??;
            reader_task.await?;

            return Ok(());
        }

        #[cfg(not(feature = "cronet-backend"))]
        {
            anyhow::bail!("cronet-rs backend was not enabled at compile time. Please compile with `--features cronet-backend`");
        }
    }
}

