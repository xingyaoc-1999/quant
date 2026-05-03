use anyhow::Result;
use chrono::Utc;
use common::Symbol;
use service::integrity::context::FeatureContextManager;
use std::str::FromStr;
use std::sync::Arc;
use storage::postgres::Storage;
use teloxide::{
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup},
    utils::command::BotCommands,
};

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase")]
pub enum MyCommand {
    #[command(description = "开始使用")]
    Start,
    #[command(description = "帮助信息")]
    Help,
    #[command(description = "订阅交易对")]
    Subscribe(String),
    #[command(description = "查看当前订阅")]
    MySubs,
    #[command(description = "系统状态")]
    Status,
}

impl MyCommand {
    pub async fn handle(
        self,
        bot: &Bot,
        chat_id: ChatId,
        storage: Arc<Storage>,
        ctx_manager: Arc<FeatureContextManager>,
    ) -> Result<()> {
        let telegram_id = chat_id.0;

        match self {
            Self::Start => {
                storage.ensure_user(telegram_id).await.unwrap_or_default();
                bot.send_message(
                    chat_id,
                    "👋 欢迎使用量化交易机器人！\n使用 /help 查看可用命令。",
                )
                .await?;
            }
            Self::Help => {
                bot.send_message(chat_id, Self::descriptions().to_string())
                    .await?;
            }
            Self::Subscribe(input) => {
                storage.ensure_user(telegram_id).await.unwrap_or_default();

                if input.trim().is_empty() {
                    let all_symbols = Symbol::all();
                    let mut buttons: Vec<Vec<InlineKeyboardButton>> = Vec::new();

                    for chunk in all_symbols.chunks(3) {
                        let row = chunk
                            .iter()
                            .map(|s| {
                                InlineKeyboardButton::callback(
                                    s.as_str(),
                                    format!("sub_{}", s.as_str()),
                                )
                            })
                            .collect();
                        buttons.push(row);
                    }

                    let keyboard = InlineKeyboardMarkup::new(buttons);
                    bot.send_message(chat_id, "📋 请选择要订阅的交易对：")
                        .reply_markup(keyboard)
                        .await?;
                    return Ok(());
                }

                match Symbol::from_str(&input) {
                    Ok(symbol) => match storage.subscribe_symbol(telegram_id, symbol).await {
                        Ok(()) => {
                            bot.send_message(
                                chat_id,
                                format!(
                                    "✅ 已订阅 {symbol}\n\
                                         —— 你将收到 {symbol} 的实时交易信号。\n\
                                         · 使用 /mysubs 查看当前订阅\n\
                                         · 使用 /help 获取更多命令",
                                ),
                            )
                            .await?;
                        }
                        Err(e) => {
                            bot.send_message(chat_id, format!("❌ 订阅失败: {e}"))
                                .await?;
                        }
                    },
                    Err(_) => {
                        bot.send_message(
                            chat_id,
                            format!(
                                "❌ 无效交易对: {input}\n\
                                 请使用有效的交易对，如 BTCUSDT\n\
                                 发送 /subscribe 查看所有可订阅列表",
                            ),
                        )
                        .await?;
                    }
                }
            }

            Self::MySubs => match storage.get_subscribed_symbols(telegram_id).await {
                Ok(symbols) => {
                    if symbols.is_empty() {
                        bot.send_message(chat_id, "📭 你当前没有订阅任何交易对。")
                            .await?;
                    } else {
                        let list = symbols
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        bot.send_message(chat_id, format!("📋 当前订阅: {}", list))
                            .await?;
                    }
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("❌ 查询失败: {}", e))
                        .await?;
                }
            },
            Self::Status => {
                let now = Utc::now().timestamp_millis();
                let status = ctx_manager.get_status_info();

                if status.is_empty() {
                    bot.send_message(chat_id, "📭 暂未监控任何交易对。").await?;
                } else {
                    let mut msg = String::from("📊 **实时系统状态**\n\n");
                    for (symbol, last_ts, count, latch, dir) in &status {
                        let seconds_ago = (now - *last_ts as i64) / 1000;
                        let direction_str = match dir {
                            Some(d) => format!("{:?}", d),
                            None => "None".into(),
                        };
                        msg.push_str(&format!(
                            "`{}`\n  ⏱ {}秒前 | 方向: {} | 计数: {}/{} | 锁存: {}\n",
                            symbol.as_str(),
                            seconds_ago,
                            direction_str,
                            count,
                            ctx_manager.signal_config.confirm_bars,
                            latch,
                        ));
                    }
                    bot.send_message(chat_id, &msg).await?;
                }
            }
        }
        Ok(())
    }
}
