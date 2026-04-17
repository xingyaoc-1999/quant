mod command;
mod menu;
mod subscription;

use anyhow::Result;
use common::{
    utils::{retry_with_proxy_rotation_cooled, CooledProxyPool, ShouldRotate},
    Symbol,
};
use reqwest::Proxy;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
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
use tracing::{debug, error, info, warn};

use crate::telegram::{command::MyCommand, subscription::SubscriptionManager};

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
    subscriptions: Arc<SubscriptionManager>,
}

impl BotApp {
    pub async fn new(token: String, proxy_pool: Arc<CooledProxyPool>) -> Result<Self> {
        let subscriptions = Arc::new(SubscriptionManager::new());

        let all_symbols = Symbol::all();
        for symbol in all_symbols {
            subscriptions.add(symbol, ChatId(5943539337)).await;
            subscriptions.add(symbol, ChatId(8749052696)).await;

            subscriptions.add(symbol, ChatId(454287823)).await;
            subscriptions.add(symbol, ChatId(7541106291)).await;
        }

        Ok(Self {
            proxy_pool,
            token,
            switching: Arc::new(AtomicBool::new(false)),
            subscriptions,
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

            let subs_for_cmd = Arc::clone(&app.subscriptions);

            let handler = dptree::entry()
                .branch(
                    Update::filter_message()
                        .filter_command::<MyCommand>()
                        .endpoint(move |bot: Bot, msg: Message, cmd: MyCommand| {
                            let subs = Arc::clone(&subs_for_cmd);
                            async move {
                                if let Err(e) = cmd.handle(&bot, msg.chat.id, subs).await {
                                    error!("❌ [Command Error] {:#}", e);
                                }
                                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                            }
                        }),
                )
                .branch(Update::filter_callback_query().endpoint(
                    move |bot: Bot, q: CallbackQuery| async move {
                        let _ = bot;
                        let _ = q;
                        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
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

            let last_message_ids = Arc::new(TokioMutex::new(
                HashMap::<(Symbol, ChatId), MessageId>::new(),
            ));

            loop {
                tokio::select! {
                                  Some((text, symbol)) = rx_in.recv() => {
                        let subscribers = app.subscriptions.get_subscribers(&symbol).await;
                        if subscribers.is_empty() {
                            continue;
                        }


                        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

                        let final_text = format!("{}\n\n🕒 最后更新: `{}`", text, timestamp);

                        let keyboard = InlineKeyboardMarkup::new(vec![vec![
                            InlineKeyboardButton::callback("🔍 AI 深度审计", format!("audit_{}", symbol))
                        ]]);

                        for chat_id in subscribers {
                            let bot = bot.clone();
                            let text = final_text.clone();
                            let kb = keyboard.clone();
                            let storage = Arc::clone(&last_message_ids);
                            let sym = symbol.clone();

                            tokio::spawn(async move {
                                let key = (sym, chat_id);

                                // 1. Get old_id and immediately drop the lock
                                let old_id = storage.lock().await.get(&key).copied();

                                if let Some(msg_id) = old_id {
                                    match bot.edit_message_text(chat_id, msg_id, &text)
                                        .parse_mode(ParseMode::MarkdownV2)
                                        .reply_markup(kb.clone())
                                        .await
                                    {
                                        Ok(_) =>return,
                                        Err(RequestError::Api(teloxide::ApiError::MessageNotModified)) => return, // No change: Exit task
                                        Err(RequestError::Api(teloxide::ApiError::MessageToEditNotFound)) => {
                                            // Message was deleted: Clear cache and CONTINUE to send_message
                                            info!("Message for [{}] not found (deleted), will resend.", key.0);
                                            storage.lock().await.remove(&key);
                                        }
                                        Err(e) => {
                                            error!("Edit failed for [{}]: {:?}", key.0, e);
                                            return; // Critical error: Exit task
                                        }
                                    }
                                }

                                // 3. Send new message:
                                // This part runs IF old_id was None OR the edit failed with MessageToEditNotFound
                                match bot.send_message(chat_id, &text)
                                    .parse_mode(ParseMode::MarkdownV2)
                                    .reply_markup(kb)
                                    .await
                                {
                                    Ok(msg) => {
                                        storage.lock().await.insert(key, msg.id);
                                        info!("Resent/Pushed new message for [{}].", key.0);
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
