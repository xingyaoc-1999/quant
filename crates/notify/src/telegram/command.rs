use anyhow::Result;
use chrono::Utc;
use common::Symbol;
use quant::{analyzer::ContextKey, types::gravity::PriceGravityWell};
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
    #[command(description = "查询交易对")]
    Check(String),
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
                let all_status = ctx_manager.get_status_info();

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

                        // 获取当前价格
                        let current_price = ctx_manager
                            .symbol_contexts
                            .get(symbol)
                            .and_then(|ctx| ctx.latest_snap.read().ok().map(|s| s.last_price))
                            .unwrap_or(0.0);
                        let price_str = format!("${:.2}", current_price);

                        let (resistance, support) = ctx_manager
                            .get_market_context(*symbol)
                            .map(|mc| {
                                mc.get_cached::<Vec<PriceGravityWell>>(
                                    ContextKey::SpaceGravityWells,
                                )
                                .cloned()
                                .unwrap_or_default()
                            })
                            .map(|wells| {
                                let nearest_res = wells
                                    .iter()
                                    .filter(|w| w.is_active && w.level > current_price)
                                    .min_by(|a, b| {
                                        (a.level - current_price)
                                            .total_cmp(&(b.level - current_price))
                                    });
                                let nearest_sup = wells
                                    .iter()
                                    .filter(|w| w.is_active && w.level < current_price)
                                    .min_by(|a, b| {
                                        (current_price - a.level)
                                            .total_cmp(&(current_price - b.level))
                                    });
                                (
                                    nearest_res
                                        .map(|w| format!("${:.2}", w.level))
                                        .unwrap_or_else(|| "—".into()),
                                    nearest_sup
                                        .map(|w| format!("${:.2}", w.level))
                                        .unwrap_or_else(|| "—".into()),
                                )
                            })
                            .unwrap_or_else(|| ("—".into(), "—".into()));

                        let symbol_esc = escape(symbol.as_str());
                        let price_esc = escape(&price_str);
                        let last_esc = escape(&last_str);
                        let current_esc = escape(&current_str);
                        let latch_esc = escape(&latch_status);
                        let res_esc = escape(&resistance);
                        let sup_esc = escape(&support);

                        msg.push_str(&format!(
                            "`{symbol}`  ⏱ {age}s ago  💵 {price}\n\
                 📌 历史方向: `{hist}`  当前方向: `{curr}`\n\
                 📈 计数: `{count}/{need}`  {latch}\n\
                 🧲 阻力: {res}  支撑: {sup}\n\n",
                            symbol = symbol_esc,
                            age = seconds_ago,
                            price = price_esc,
                            hist = last_esc,
                            curr = current_esc,
                            count = count,
                            need = ctx_manager.signal_config.confirm_bars,
                            latch = latch_esc,
                            res = res_esc,
                            sup = sup_esc,
                        ));
                    }

                    let stats = ctx_manager.stats.lock().await;
                    let avg_rr = if stats.recent_rrs.is_empty() {
                        0.0
                    } else {
                        stats.recent_rrs.iter().sum::<f64>() / stats.recent_rrs.len() as f64
                    };

                    msg.push_str("───────\n");
                    msg.push_str(&format!(
                        "📊 今日信号: `{}` \\| 被拒: `{}` \\| 更新: `{}` \\| 平均RR: `{:.2}`\n",
                        stats.signal_count, stats.reject_count, stats.update_count, avg_rr
                    ));

                    for sym in &subscribed {
                        if let Some(reason) = stats.reject_reasons.get(sym) {
                            msg.push_str(&format!(
                                "`{}`  ⚠️ 上次拒绝: {}\n",
                                escape(sym.as_str()),
                                escape(reason)
                            ));
                        }
                    }

                    msg.push_str(&format!("📡 当前订阅交易对: {}", subscribed.len()));

                    bot.send_message(chat_id, &msg)
                        .parse_mode(ParseMode::MarkdownV2)
                        .await?;
                }
            }
            Self::Help => {
                bot.send_message(chat_id, Self::descriptions().to_string())
                    .await?;
            }

            Self::Check(input) => {
                let symbol = match Symbol::from_str(&input) {
                    Ok(s) => s,
                    Err(_) => {
                        bot.send_message(chat_id, format!("❌ 无效交易对: {}", escape(&input)))
                            .await?;
                        return Ok(());
                    }
                };

                let mut ctx = match ctx_manager.get_market_context(symbol) {
                    Some(c) => c,
                    None => {
                        bot.send_message(chat_id, "📭 该交易对暂无市场数据，请稍后再试。")
                            .await?;
                        return Ok(());
                    }
                };
                // let audit = engine.run(&mut ctx);
            }
        }
        Ok(())
    }
}
