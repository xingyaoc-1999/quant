mod command;
mod menu;

use anyhow::{Context, Result};
use api_client::http::binance::ArchiveProvider;
use common::{
    utils::{retry_with_proxy_rotation_cooled, CooledProxyPool, ShouldRotate},
    Interval, Symbol,
};
use quant::analyzer::Role;
use reqwest::Proxy;
use service::context::FeatureContextManager;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::{collections::HashMap, str::FromStr};
use storage::postgres::Storage;
use teloxide::{
    dispatching::{Dispatcher, HandlerExt, UpdateFilterExt},
    prelude::*,
    types::{CallbackQuery, Message, ParseMode, Update},
    update_listeners,
    utils::command::BotCommands,
    Bot, RequestError,
};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::{error, info, warn};

use crate::telegram::command::MyCommand;

struct BotRotator;
impl ShouldRotate<RequestError> for BotRotator {
    fn should_rotate(&self, error: &RequestError) -> bool {
        BotApp::is_network_error(error)
    }
}

pub struct BotApp {
    proxy_pool: Arc<CooledProxyPool>,
    token: String,
    switching: Arc<AtomicBool>,
}

impl BotApp {
    pub async fn new(token: String, proxy_pool: Arc<CooledProxyPool>) -> Result<Self> {
        Ok(Self {
            proxy_pool,
            token,
            switching: Arc::new(AtomicBool::new(false)),
        })
    }

    async fn create_bot_with_available_proxy(&self) -> Result<Bot, RequestError> {
        let token = self.token.clone();
        let request_fn = move |proxy| {
            let token = token.clone();
            async move {
                let mut client_builder = teloxide::net::default_reqwest_settings();
                if let Some(proxy_addr) = proxy {
                    let proxy = Proxy::all(format!("socks5h://{}", proxy_addr))
                        .map_err(|e| RequestError::Network(Arc::new(e)))?;
                    client_builder = client_builder.proxy(proxy);
                }
                let client = client_builder
                    .build()
                    .map_err(|e| RequestError::Network(Arc::new(e)))?;
                let bot = Bot::with_client(token, client);
                bot.get_me().await?;
                Ok(bot)
            }
        };

        retry_with_proxy_rotation_cooled(&self.proxy_pool, request_fn, BotRotator).await
    }

    pub async fn run(
        &self,
        tx_in: Sender<(String, ChatId)>,
        mut rx_in: Receiver<(String, ChatId)>,
        manager: Arc<FeatureContextManager>,
        archive_provider: Arc<ArchiveProvider>,
        storage: Arc<Storage>,
    ) -> Result<()> {
        let app = Arc::new(self);

        loop {
            info!("🚀 Creating bot with available proxy...");
            let bot = match app.create_bot_with_available_proxy().await {
                Ok(bot) => bot,
                Err(e) => {
                    error!("❌ All proxies failed: {:?}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                    continue;
                }
            };

            // 同步指令菜单
            let _ = bot.set_my_commands(MyCommand::bot_commands()).await;
            info!("SUCCESS [Bot] Telegram commands synced to server.");

            let (switch_tx, mut switch_rx) = mpsc::channel(1);
            let app_inner = Arc::clone(&app);

            // 错误处理器：检测网络错误并触发代理切换
            let switching_err = app_inner.switching.clone();
            let switch_tx_err = switch_tx.clone();
            let error_handler = Arc::new(move |error: RequestError| {
                let switching = switching_err.clone();
                let switch_tx = switch_tx_err.clone();
                async move {
                    error!("🚨 TG Error Captured: {:?}", error);
                    if BotApp::is_network_error(&error)
                        && switching
                            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                    {
                        warn!("🌐 Network error detected, signaling proxy switch...");
                        let _ = switch_tx.try_send(());
                    }
                }
            });

            let manager_for_cmd = manager.clone();
            let tx_in_msg = tx_in.clone();
            let storage = storage.clone();
            let manager_for_cb = manager.clone();
            let provider_for_cb = archive_provider.clone();
            let handler = dptree::entry()
                .branch(
                    Update::filter_message()
                        .filter_command::<MyCommand>()
                        .endpoint(move |bot: Bot, msg: Message, cmd: MyCommand| {
                            let manager = manager_for_cmd.clone();
                            let tx_in = tx_in_msg.clone();
                            async move {
                                if let Err(e) = cmd.handle(&bot, msg.chat.id, tx_in, manager).await
                                {
                                    error!("❌ [Command Error] {:#}", e);
                                }
                                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                            }
                        }),
                )
                .branch(Update::filter_callback_query().endpoint(
                    move |bot: Bot, q: CallbackQuery| {
                        let manager = manager_for_cb.clone();
                        let storage = storage.clone();
                        let provider = provider_for_cb.clone();
                        async move {
                            if let Err(e) =
                                BotApp::handle_callback_query(bot, q, manager, provider, storage)
                                    .await
                            {
                                error!("❌ [Callback Error] {:#}", e);
                            }
                            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                        }
                    },
                ));
            let mut dispatcher = Dispatcher::builder(bot.clone(), handler)
                .enable_ctrlc_handler()
                .build();

            let shutdown_token = dispatcher.shutdown_token();
            let bot_polling = bot.clone();

            let mut dispatch_task = tokio::spawn(async move {
                dispatcher
                    .dispatch_with_listener(
                        update_listeners::polling_default(bot_polling).await,
                        error_handler,
                    )
                    .await
            });

            loop {
                tokio::select! {
                    Some((_text, _chat_id)) = rx_in.recv() => {
                    }
                    _ = switch_rx.recv() => {
                        info!("🔄 Switch signal received, restarting dispatcher...");
                        let _ = shutdown_token.shutdown();
                        let _ = dispatch_task.await;
                        break;
                    }
                    result = &mut dispatch_task => {
                        match result {
                            Ok(_) => { info!("Dispatcher finished."); return Ok(()); }
                            Err(e) => { error!("Dispatcher Task Panic: {:?}", e); break; }
                        }
                    }
                }
            }

            app.switching.store(false, Ordering::SeqCst);
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }
    async fn handle_callback_query(
        bot: Bot,
        q: CallbackQuery,
        manager: Arc<FeatureContextManager>,
        archive_provider: Arc<ArchiveProvider>,
        storage: Arc<Storage>,
    ) -> Result<()> {
        let data = q.data.as_ref().context("Callback data is missing")?;
        let (chat_id, msg_id) = q
            .message
            .as_ref()
            .map(|m| (m.chat().id, m.id()))
            .context("Callback message is missing or inaccessible")?;

        bot.answer_callback_query(q.id).await?;

        // 处理返回主菜单
        if data == "LST:BACK" {
            bot.delete_message(chat_id, msg_id).await?;
            Self::send_config_list(&bot, chat_id, manager).await?;
            return Ok(());
        }

        // 1. 进入特定 Role 的周期选择界面
        if let Some(payload) = data.strip_prefix("INT:") {
            let parts: Vec<&str> = payload.split(':').collect();
            if parts.len() == 2 {
                let (symbol, role) = (parts[0], parts[1]);
                bot.edit_message_text(
                    chat_id,
                    msg_id,
                    format!("⚙️ 设置 *{}* 的 *{}* 周期:", symbol, role),
                )
                .parse_mode(ParseMode::MarkdownV2)
                .reply_markup(Self::make_interval_keyboard(symbol, role))
                .await?;
            }
        } else if let Some(payload) = data.strip_prefix("SET:") {
            let parts: Vec<&str> = payload.split(':').collect();
            if parts.len() == 3 {
                let (s_str, r_str, i_str) = (parts[0], parts[1], parts[2]);

                let symbol = Symbol::from_str(s_str)
                    .map_err(anyhow::Error::msg)
                    .context("Invalid symbol")?;

                let role = r_str
                    .parse::<Role>()
                    .map_err(anyhow::Error::msg)
                    .context("Invalid role")?;

                let interval = Interval::from_str(i_str)
                    .map_err(anyhow::Error::msg)
                    .context("Invalid interval")?;

                let mut config = HashMap::new();
                config.insert(role, interval);
                let changed_roles = manager.update_symbol_config(symbol, config);

                let provider = archive_provider.clone();

                // ... 前面解析 symbol, role, interval 的代码保持不变 ...

                if !changed_roles.is_empty() {
                    for (changed_role, target_interval) in changed_roles {
                        let m_clone = manager.clone();
                        let s_clone = storage.clone();
                        let p_clone = archive_provider.clone();
                        let symbol_clone = symbol;

                        tokio::spawn(async move {
                            info!(
                                "🚀 [Warmup] Starting: {} | {} -> {:?}",
                                symbol_clone, changed_role, target_interval
                            );

                            let res: anyhow::Result<()> = async {
                // 1. 必填数据：只 try_join K 线数据，如果 K 线都拿不到，那预热没法做
                let (seeds_res, m1_res) = tokio::try_join!(
                    s_clone.get_latest_candles(&symbol_clone, target_interval, 200),
                    s_clone.get_latest_candles(&symbol_clone, Interval::M1, 1000),
                )?;

                // 2. 选填数据：单独抓取 OI，失败仅打印警告，不中断流程
                let oi_res = p_clone.fetch_open_interest_hist(symbol_clone, target_interval).await;

                // 3. 组装 Seeds Map
                let mut seeds_map = HashMap::new();
                seeds_map.insert(target_interval, seeds_res);
                seeds_map.insert(Interval::M1, m1_res);

                // 4. 组装 OI Map (关键：适配 Option 签名)
                let mut oi_map = HashMap::new();
                let oi_param = match oi_res {
                    Ok(data) => {
                        oi_map.insert(target_interval, data);
                        Some(&oi_map) // 抓取成功，传数据
                    }
                    Err(e) => {
                        warn!("⚠️ [Warmup] OI fetch failed for {}, proceeding with K-lines only: {:?}", symbol_clone, e);
                        None // 抓取失败或没数据，传 None
                    }
                };

                // 5. 执行预热 (现在的实现即便 oi_param 是 None 也会算出指标)
                m_clone.warmup_single_symbol(symbol_clone, &seeds_map, oi_param);

                info!("✅ [Warmup] Success: {}'s {} updated", symbol_clone, changed_role);
                Ok(())
            }
            .await;

                            if let Err(e) = res {
                                error!(
                                    "❌ [Warmup] Critical error for {} {}: {:?}",
                                    symbol_clone, changed_role, e
                                );
                            }
                        });
                    }
                }

                bot.delete_message(chat_id, msg_id).await?;
                Self::send_config_list(&bot, chat_id, manager).await?;
            }
        }

        Ok(())
    }

    pub fn is_network_error(e: &RequestError) -> bool {
        matches!(e, RequestError::Network(_) | RequestError::Io(_))
    }
}
