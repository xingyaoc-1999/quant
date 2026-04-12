use crate::telegram::subscription::SubscriptionManager;
use anyhow::Result;
use common::Symbol;
use std::sync::Arc;
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
    #[command(description = "取消订阅")]
    Unsubscribe { symbol: Symbol },
}

impl MyCommand {
    pub async fn handle(
        self,
        bot: &Bot,
        chat_id: ChatId,

        subs: Arc<SubscriptionManager>,
    ) -> Result<()> {
        match self {
            Self::Start => {
                bot.send_message(chat_id, "👋 欢迎！使用 /subscribe BTCUSDT 订阅信号。")
                    .await?;
            }
            Self::Help => {
                bot.send_message(chat_id, Self::descriptions().to_string())
                    .await?;
            }
            Self::Subscribe { symbol } => {
                subs.add(symbol, chat_id).await;
                bot.send_message(chat_id, format!("✅ 已订阅 {}", symbol))
                    .await?;
            }
            Self::Unsubscribe { symbol } => {
                if subs.remove(&symbol, chat_id).await {
                    bot.send_message(chat_id, format!("❌ 已取消订阅 {}", symbol))
                        .await?;
                } else {
                    bot.send_message(chat_id, format!("⚠️ 你未订阅 {}", symbol))
                        .await?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
