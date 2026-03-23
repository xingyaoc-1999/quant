use anyhow::{anyhow, bail, Result};
use common::utils::CooledProxyPool;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashSet;
use std::hash::Hash;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::mpsc::Sender;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tokio_socks::tcp::Socks5Stream;
use tokio_tungstenite::{
    client_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, Message},
    MaybeTlsStream, WebSocketStream,
};
use tracing::{info, warn};
pub mod biance;

type Sink = Arc<Mutex<Option<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>>>;

pub trait WsProtocol: Send + Sync + 'static {
    type Subscription: Clone + Eq + Hash + Send + Sync + 'static;
    type Output: Send + 'static;
    fn url(&self) -> &str;
    fn proxy_target(&self) -> &str;
    fn build_subscribe_request(&self, subs: &[Self::Subscription]) -> Result<String>;
    fn parse_message(&self, text: &str) -> Option<Self::Output>;
}
pub struct GenericWsClient<P: WsProtocol> {
    protocol: Arc<P>,
    proxy_pool: Arc<CooledProxyPool>,
    write: Sink,
    current_subscriptions: Arc<Mutex<HashSet<P::Subscription>>>,
}

impl<P: WsProtocol> GenericWsClient<P> {
    pub fn new(
        protocol: P,
        proxy_pool: Arc<CooledProxyPool>,
        initial_subs: HashSet<P::Subscription>,
    ) -> Self {
        Self {
            protocol: Arc::new(protocol),
            proxy_pool,
            write: Arc::new(Mutex::new(None)),
            current_subscriptions: Arc::new(Mutex::new(initial_subs)),
        }
    }

    async fn connect(
        &self,
    ) -> Result<(
        SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
        SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    )> {
        let proxy_addr = match self.proxy_pool.current_available().await {
            Some(proxy) => proxy,
            None => bail!("No available proxy in pool"),
        };

        match Socks5Stream::connect(&*proxy_addr, self.protocol.proxy_target()).await {
            Ok(stream) => {
                let tcp = stream.into_inner();
                let req = self.protocol.url().into_client_request()?;
                let (ws, _) = client_async_tls_with_config(req, tcp, None, None).await?;
                Ok(ws.split())
            }
            Err(e) => {
                self.proxy_pool.mark_failed(proxy_addr.clone()).await;
                bail!("Proxy {} connection failed: {}", proxy_addr, e);
            }
        }
    }

    pub async fn subscribe(&self, sub: P::Subscription) -> Result<()> {
        {
            let mut subs = self.current_subscriptions.lock().await;
            if !subs.insert(sub.clone()) {
                return Ok(());
            }
        }

        let request_str = self.protocol.build_subscribe_request(&[sub])?;

        let mut write_guard = self.write.lock().await;
        if let Some(write) = write_guard.as_mut() {
            write
                .send(Message::Text(request_str.into()))
                .await
                .map_err(|e| anyhow!("Failed to send subscribe request: {}", e))?;
        }

        Ok(())
    }

    async fn resubscribe(&self) -> Result<()> {
        let subs = {
            let lock = self.current_subscriptions.lock().await;
            lock.iter().cloned().collect::<Vec<_>>()
        };

        if !subs.is_empty() {
            let req_str = self
                .protocol
                .build_subscribe_request(&subs)
                .map_err(|e| anyhow!("Protocol build error: {:?}", e))?;

            let mut lock = self.write.lock().await;
            if let Some(w) = lock.as_mut() {
                w.send(Message::Text(req_str.into()))
                    .await
                    .map_err(|e| anyhow!("Send subscribe failed: {:?}", e))?;
            }
        }
        Ok(())
    }
    async fn handle_message(&self, msg: Message, tx: &Sender<P::Output>) -> Result<()> {
        match msg {
            Message::Text(text) => {
                if let Some(output) = self.protocol.parse_message(&text) {
                    tx.send(output)
                        .await
                        .map_err(|_| anyhow!("channel closed"))?;
                }
            }
            Message::Ping(data) => {
                let mut write_guard = self.write.lock().await;
                if let Some(write) = write_guard.as_mut() {
                    let _ = write.send(Message::Pong(data)).await;
                }
            }
            Message::Close(frame) => {
                info!("📡 [WS] Received close frame: {:?}", frame);
                return Err(anyhow!("Connection closed by remote"));
            }
            _ => {}
        }
        Ok(())
    }
    pub async fn run(&self, tx: Sender<P::Output>) -> Result<()> {
        const MAX_DELAY: Duration = Duration::from_secs(30);
        let mut retry_count = 0;

        loop {
            let (write, mut read) = match self.connect().await {
                Ok(pair) => {
                    retry_count = 0; // 连接成功，重置重试计数
                    pair
                }
                Err(e) => {
                    retry_count += 1;
                    let delay = Duration::from_secs(2_u64.pow(retry_count.min(5))).min(MAX_DELAY);
                    warn!("📡 [WS] Connect failed: {}. Retrying in {:?}...", e, delay);
                    sleep(delay).await;
                    continue;
                }
            };

            {
                let mut lock = self.write.lock().await;
                *lock = Some(write);
            }

            info!("🚀 [WS] Connected and ready. Sending initial subscriptions...");

            if let Err(e) = self.resubscribe().await {
                warn!("⚠️ [WS] Resubscribe failed: {:?}. Reconnecting...", e);
                continue;
            }

            while let Some(result) = read.next().await {
                match result {
                    Ok(msg) => {
                        if let Err(e) = self.handle_message(msg, &tx).await {
                            if e.to_string().contains("channel closed") {
                                return Err(anyhow!("Receiver dropped, shutting down WS engine"));
                            }
                            warn!("⚠️ [WS] Message handling error: {:?}", e);
                        }
                    }
                    Err(e) => {
                        warn!("📡 [WS] Stream connection broken: {:?}", e);
                        break;
                    }
                }
            }

            info!("🔄 [WS] Connection lost. Cleaning up and preparing to reconnect...");
            {
                let mut lock = self.write.lock().await;
                *lock = None;
            }
            sleep(Duration::from_secs(1)).await;
        }
    }
}
