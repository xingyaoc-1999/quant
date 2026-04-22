use anyhow::Result;
use common::Symbol;
use std::sync::Arc;
use storage::postgres::Storage;
use teloxide::{prelude::*, utils::command::BotCommands};

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase")]
pub enum MyCommand {
    #[command(description = "开始使用")]
    Start,
    #[command(description = "帮助信息")]
    Help,
    #[command(description = "订阅交易对")]
    Subscribe { symbol: Symbol },

    #[command(description = "查看当前订阅")]
    MySubs,
}

impl MyCommand {
    pub async fn handle(self, bot: &Bot, chat_id: ChatId, storage: Arc<Storage>) -> Result<()> {
        let telegram_id = chat_id.0;

        match self {
            Self::Start => {
                storage
                    .ensure_user(telegram_id, None, None)
                    .await
                    .unwrap_or_default();
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
            Self::Subscribe { symbol } => {
                storage
                    .ensure_user(telegram_id, None, None)
                    .await
                    .unwrap_or_default();

                match storage.subscribe_symbol(telegram_id, symbol).await {
                    Ok(()) => {
                        bot.send_message(chat_id, format!("✅ 已订阅 {}", symbol))
                            .await?;
                    }
                    Err(e) => {
                        bot.send_message(chat_id, format!("❌ 订阅失败: {}", e))
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
        }
        Ok(())
    }
}
