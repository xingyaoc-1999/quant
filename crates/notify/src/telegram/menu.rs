use std::sync::Arc;

use crate::telegram::BotApp;
use common::{Interval, Symbol};
use quant::analyzer::Role;
use service::context::FeatureContextManager;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, Message, ParseMode};

impl BotApp {
    pub async fn send_config_list(
        bot: &Bot,
        chat_id: ChatId,
        feature_contexts: Arc<FeatureContextManager>,
    ) -> Result<Message, teloxide::RequestError> {
        let mut display_data: Vec<(Symbol, Role, Interval)> = Vec::new();

        for entry in feature_contexts.symbol_contexts.iter() {
            let symbol = entry.key();
            let context = entry.value();
            let roles_guard = context.roles.read().expect("Lock poisoned");

            // 2. 遍历锁内部的 HashMap
            for (role, processor) in roles_guard.iter() {
                display_data.push((*symbol, *role, processor.interval));
            }
        }

        display_data.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        if display_data.is_empty() {
            return bot
                .send_message(chat_id, "📭 *当前无任何配置*\n使用 `/add` 开始")
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await;
        }

        let keyboard = display_data
            .into_iter()
            .map(|(symbol, role, interval)| {
                let btn_text = format!("{} {}: {}", role.icon(), symbol, interval.as_str());
                let callback = format!("INT:{}:{:?}", symbol, role);
                InlineKeyboardButton::callback(btn_text, callback)
            })
            .collect::<Vec<_>>()
            .chunks(3)
            .map(|chunk| chunk.to_vec())
            .collect::<Vec<Vec<_>>>();

        bot.send_message(chat_id, "📋 *配置看板*\n点击按钮快速修改周期：")
            .parse_mode(ParseMode::MarkdownV2)
            .reply_markup(InlineKeyboardMarkup::new(keyboard))
            .await
    }

    pub fn make_interval_keyboard(symbol: &str, role: &str) -> InlineKeyboardMarkup {
        let mut buttons: Vec<Vec<InlineKeyboardButton>> = Interval::all()
            .chunks(3)
            .map(|chunk| {
                chunk
                    .iter()
                    .map(|int| {
                        InlineKeyboardButton::callback(
                            int.as_str(),
                            format!("SET:{}:{}:{}", symbol, role, int.as_str()),
                        )
                    })
                    .collect()
            })
            .collect();
        buttons.push(vec![InlineKeyboardButton::callback(
            "⬅️ 返回列表",
            "LST:BACK",
        )]);
        InlineKeyboardMarkup::new(buttons)
    }
}
