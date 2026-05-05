use anyhow::Result;
use chrono::Utc;
use common::Symbol;
use service::integrity::context::FeatureContextManager;
use std::str::FromStr;
use std::sync::Arc;
use storage::postgres::Storage;
use teloxide::{
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, ParseMode},
    utils::{command::BotCommands, markdown::escape},
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

            Self::Status => {
                let subscribed = storage
                    .get_subscribed_symbols(telegram_id)
                    .await
                    .unwrap_or_default();

                if subscribed.is_empty() {
                    bot.send_message(chat_id, "📭 你当前没有订阅任何交易对。")
                        .await?;
                    return Ok(());
                }

                let now = Utc::now().timestamp_millis();
                let all_status = ctx_manager.get_status_info(); // 返回 (Symbol, i64, usize, bool, Option<TradeDirection>, Option<TradeDirection>, usize)

                let user_status: Vec<_> = all_status
                    .into_iter()
                    .filter(|(sym, ..)| subscribed.contains(sym))
                    .collect();

                if user_status.is_empty() {
                    bot.send_message(chat_id, "📭 你所订阅的交易对暂无实时状态。")
                        .await?;
                } else {
                    let mut msg = String::from("*📊 实时系统状态*\n\n");

                    for (symbol, last_ts, count, latch, last_dir, current_dir, latch_remain) in
                        &user_status
                    {
                        let seconds_ago = (now - *last_ts as i64) / 1000;

                        let last_str = match last_dir {
                            Some(d) => format!("{:?}", d),
                            None => "—".into(),
                        };
                        let current_str = match current_dir {
                            Some(d) => format!("{:?}", d),
                            None => "—".into(),
                        };

                        let latch_status = if *latch {
                            format!("🔒 锁存中 (剩余 {} 根)", latch_remain)
                        } else {
                            "🔓 未锁存".into()
                        };

                        let price_str = ctx_manager
                            .symbol_contexts
                            .get(symbol)
                            .and_then(|ctx| ctx.latest_snap.read().ok().map(|s| s.last_price))
                            .map(|p| format!("${:.2}", p))
                            .unwrap_or_else(|| "—".into());

                        let symbol_esc: String = escape(symbol.as_str());
                        let price_esc = escape(&price_str);
                        let last_esc = escape(&last_str);
                        let current_esc = escape(&current_str);
                        let latch_esc = escape(&latch_status);

                        msg.push_str(&format!(
                            "`{symbol}`  ⏱ {age}s ago  💵 {price}\n\
                 \x20\x20📌 历史方向: `{hist}`  当前方向: `{curr}`\n\
                 \x20\x20📈 计数: `{count}/{need}`  {latch}\n\n",
                            symbol = symbol_esc,
                            age = seconds_ago,
                            price = price_esc,
                            hist = last_esc,
                            curr = current_esc,
                            count = count,
                            need = ctx_manager.signal_config.confirm_bars,
                            latch = latch_esc,
                        ));
                    }

                    msg.push_str("───────\n");
                    msg.push_str(&format!(
                        "⚙️ 确认需连续 `{}` 根 \\| 锁存 `{}` 根\n",
                        ctx_manager.signal_config.confirm_bars,
                        ctx_manager.signal_config.latch_bars,
                    ));
                    msg.push_str(&format!(
                        "📡 总监控交易对: {}",
                        ctx_manager.symbol_contexts.len(), // 系统实际监控总数
                    ));

                    bot.send_message(chat_id, &msg)
                        .parse_mode(ParseMode::MarkdownV2)
                        .await?;
                }
            }
        }
        Ok(())
    }
}
