// app.rs 或 bot.rs
mod callback;
mod command;
use anyhow::Result;
use chrono::{FixedOffset, Utc};
use common::{
    utils::{retry_with_proxy_rotation_cooled, CooledProxyPool, ShouldRotate},
    Symbol,
};
use quant::{analyzer::AnalysisEngine, config::AnalyzerConfig};
use reqwest::Proxy;
use service::integrity::context::FeatureContextManager;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use storage::postgres::Storage;
use teloxide::{
    dispatching::{Dispatcher, HandlerExt, UpdateFilterExt},
    prelude::*,
    types::{
        CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, Message, MessageId, ParseMode,
        Update,
    },
    update_listeners,
    utils::command::BotCommands,
    Bot, RequestError,
};
use tokio::sync::mpsc::{self, Receiver};
use tokio::sync::Mutex as TokioMutex;
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
    storage: Arc<Storage>,
    engine: Arc<AnalysisEngine>,
    ctx_manager: Arc<FeatureContextManager>,
    config: Arc<AnalyzerConfig>,
    execute_order: Arc<
        dyn Fn(
                &quant::risk_manager::RiskAssessment,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>>
            + Send
            + Sync,
    >,
}

impl BotApp {
    pub async fn new(
        token: String,
        proxy_pool: Arc<CooledProxyPool>,
        storage: Arc<Storage>,
        engine: Arc<AnalysisEngine>,
        ctx_manager: Arc<FeatureContextManager>,
        config: Arc<AnalyzerConfig>,
        execute_order: Arc<
            dyn Fn(
                    &quant::risk_manager::RiskAssessment,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Result<Self> {
        Ok(Self {
            proxy_pool,
            token,
            switching: Arc::new(AtomicBool::new(false)),
            storage,
            engine,
            ctx_manager,
            config,
            execute_order,
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

    pub async fn run(&self, mut rx_in: Receiver<(String, Symbol)>) -> Result<()> {
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

            let _ = bot.set_my_commands(MyCommand::bot_commands()).await;
            info!("[Bot] Telegram commands synced to server.");

            let (switch_tx, mut switch_rx) = mpsc::channel(1);
            let app_inner = Arc::clone(&app);

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

            // 构建回调依赖
            let callback_deps = callback::CallbackDeps {
                storage: Arc::clone(&app.storage),
                engine: Arc::clone(&app.engine),
                ctx_manager: Arc::clone(&app.ctx_manager),
                config: Arc::clone(&app.config),
                execute_order: Arc::clone(&app.execute_order),
            };

            let storage_for_cmd = Arc::clone(&app.storage);
            let handler = dptree::entry()
                .branch(
                    Update::filter_message()
                        .filter_command::<MyCommand>()
                        .endpoint(move |bot: Bot, msg: Message, cmd: MyCommand| {
                            let storage = Arc::clone(&storage_for_cmd);
                            async move {
                                if let Err(e) = cmd.handle(&bot, msg.chat.id, storage).await {
                                    error!("❌ [Command Error] {:#}", e);
                                }
                                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                            }
                        }),
                )
                .branch(Update::filter_callback_query().endpoint({
                    let deps = callback_deps;
                    move |bot: Bot, q: CallbackQuery| {
                        let deps = deps.clone();
                        async move {
                            if let Err(e) = callback::handle_callback(bot, q, deps).await {
                                error!("❌ [Callback Error] {:#}", e);
                            }
                            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                        }
                    }
                }));

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

            let last_message_ids = Arc::new(TokioMutex::new(
                HashMap::<(Symbol, ChatId), MessageId>::new(),
            ));

            loop {
                tokio::select! {
                    Some((text, symbol)) = rx_in.recv() => {
                        let subscribers = match app.storage.get_subscribed_users(symbol).await {
                            Ok(list) => list,
                            Err(e) => {
                                error!("Failed to fetch subscribers for {}: {}", symbol, e);
                                continue;
                            }
                        };
                        if subscribers.is_empty() {
                            continue;
                        }

                        let timestamp = Utc::now()
                            .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
                            .format("%Y-%m-%d %H:%M:%S")
                            .to_string();
                        let final_text = format!("{}\n\n🕒 Last update: `{}`", text, timestamp);

                        let keyboard = InlineKeyboardMarkup::new(vec![
                            vec![
                                InlineKeyboardButton::callback(
                                    "📈 执行信号",
                                    format!("exec_{}", symbol),
                                ),
                                InlineKeyboardButton::callback(
                                    "🔍 AI 审计",
                                    format!("audit_{}", symbol),
                                ),
                            ],
                            vec![
                                InlineKeyboardButton::callback(
                                    "🚫 不再提示",
                                    format!("mute_{}", symbol),
                                ),
                            ],
                        ]);

                        for telegram_id in subscribers {
                            let chat_id = ChatId(telegram_id);
                            let bot = bot.clone();
                            let text = final_text.clone();
                            let kb = keyboard.clone();
                            let storage = Arc::clone(&last_message_ids);
                            let sym = symbol.clone();

                            tokio::spawn(async move {
                                let key = (sym, chat_id);
                                let old_id = storage.lock().await.get(&key).copied();

                                if let Some(msg_id) = old_id {
                                    match bot.edit_message_text(chat_id, msg_id, &text)
                                        .parse_mode(ParseMode::MarkdownV2)
                                        .reply_markup(kb.clone())
                                        .await
                                    {
                                        Ok(_) => return,
                                        Err(RequestError::Api(teloxide::ApiError::MessageNotModified)) => return,
                                        Err(RequestError::Api(teloxide::ApiError::MessageToEditNotFound)) => {
                                            info!("Message for [{}] not found, resending.", key.0);
                                            storage.lock().await.remove(&key);
                                        }
                                        Err(e) => {
                                            error!("Edit failed for [{}]: {:?}", key.0, e);
                                            return;
                                        }
                                    }
                                }

                                match bot.send_message(chat_id, &text)
                                    .parse_mode(ParseMode::MarkdownV2)
                                    .reply_markup(kb)
                                    .await
                                {
                                    Ok(msg) => {
                                        storage.lock().await.insert(key, msg.id);
                                        info!("Sent new message for [{}].", key.0);
                                    }
                                    Err(e) => error!("Send failed for [{}]: {:?}", key.0, e),
                                }
                            });
                        }
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

    pub fn is_network_error(e: &RequestError) -> bool {
        matches!(e, RequestError::Network(_) | RequestError::Io(_))
    }
}
