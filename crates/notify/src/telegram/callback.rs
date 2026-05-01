use crate::telegram::menu::build_keyboard_for_user;
use crate::telegram::TokioMutex;
use anyhow::Result;
use common::Symbol;
use quant::analyzer::AnalysisEngine;
use quant::config::AnalyzerConfig;
use quant::risk_manager::RiskAssessment;
use service::integrity::context::FeatureContextManager;
use std::sync::Arc;
use std::{collections::HashMap, str::FromStr};
use storage::postgres::Storage;
use teloxide::{
    prelude::*,
    types::{CallbackQuery, ChatId, MessageId},
    Bot,
};

#[derive(Clone)]
pub struct CallbackDeps {
    pub storage: Arc<Storage>,
    pub engine: Arc<AnalysisEngine>,
    pub ctx_manager: Arc<FeatureContextManager>,
    pub config: Arc<AnalyzerConfig>,
    pub assessment_cache: Arc<TokioMutex<HashMap<Symbol, RiskAssessment>>>,
    pub execute_order: Arc<
        dyn Fn(
                &RiskAssessment,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>>
            + Send
            + Sync,
    >,
}

pub async fn handle_callback(
    bot: Bot,
    q: CallbackQuery,
    deps: CallbackDeps,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let data = q.data.as_deref().unwrap_or("");
    let chat_id = q.message.as_ref().map(|m| m.chat().id).unwrap_or(ChatId(0));
    let msg_id = q.message.as_ref().map(|m| m.id()).unwrap_or(MessageId(0));
    let user_id = q.from.id.0 as i64;

    match data {
        d if d.starts_with("sub_") => {
            handle_subscribe_callback(&bot, &q, d, chat_id, msg_id, &deps, user_id).await?
        }
        d if d.starts_with("exec_") => {
            handle_exec(&bot, &q, d, chat_id, msg_id, &deps, user_id).await?
        }
        d if d.starts_with("close_") => {
            handle_close(&bot, &q, d, chat_id, msg_id, &deps, user_id).await?
        }
        d if d.starts_with("mute_") => {
            handle_mute(&bot, &q, d, chat_id, msg_id, &deps, user_id).await?
        }
        d if d.starts_with("unmute_") => {
            handle_unmute(&bot, &q, d, chat_id, msg_id, &deps, user_id).await?
        }
        _ => {}
    }
    Ok(())
}

async fn handle_subscribe_callback(
    bot: &Bot,
    q: &CallbackQuery,
    data: &str,
    chat_id: ChatId,
    _msg_id: MessageId,
    deps: &CallbackDeps,
    user_id: i64,
) -> Result<()> {
    let symbol_str = data.trim_start_matches("sub_");
    let Ok(symbol) = Symbol::from_str(symbol_str) else {
        bot.answer_callback_query(q.id.clone())
            .text("无效交易对")
            .show_alert(true)
            .await?;
        return Ok(());
    };

    // 确保用户存在
    if let Err(e) = deps.storage.ensure_user(user_id).await {
        bot.answer_callback_query(q.id.clone())
            .text(format!("用户初始化失败: {e}"))
            .show_alert(true)
            .await?;
        return Ok(());
    }

    match deps.storage.subscribe_symbol(user_id, symbol).await {
        Ok(()) => {
            bot.answer_callback_query(q.id.clone())
                .text(format!("✅ 已订阅 {}", symbol))
                .show_alert(true)
                .await?;
        }
        Err(e) => {
            bot.answer_callback_query(q.id.clone())
                .text(format!("❌ 订阅失败: {e}"))
                .show_alert(true)
                .await?;
        }
    }
    Ok(())
}

async fn handle_mute(
    bot: &Bot,
    q: &CallbackQuery,
    data: &str,
    chat_id: ChatId,
    msg_id: MessageId,
    deps: &CallbackDeps,
    user_id: i64,
) -> Result<()> {
    let symbol_str = data.trim_start_matches("mute_");
    let Ok(symbol) = Symbol::from_str(symbol_str) else {
        bot.answer_callback_query(q.id.clone()).await?;
        return Ok(());
    };

    match deps.storage.mute_symbol(user_id, symbol).await {
        Ok(()) => {
            bot.answer_callback_query(q.id.clone())
                .text(format!("🔕 已静音 {}", symbol))
                .await?;

            let new_keyboard = build_keyboard_for_user(&deps.storage, user_id, symbol).await;
            bot.edit_message_reply_markup(chat_id, msg_id)
                .reply_markup(new_keyboard)
                .await?;
        }
        Err(e) => {
            let error_text = if e.to_string().contains("no subscription found") {
                format!("❌ 无法静音：您尚未订阅 {}，请先订阅。", symbol)
            } else {
                "❌ 操作失败，请稍后重试。".to_string()
            };
            bot.answer_callback_query(q.id.clone())
                .text(&error_text)
                .show_alert(true)
                .await?;
        }
    }
    Ok(())
}

async fn handle_unmute(
    bot: &Bot,
    q: &CallbackQuery,
    data: &str,
    chat_id: ChatId,
    msg_id: MessageId,
    deps: &CallbackDeps,
    user_id: i64,
) -> Result<()> {
    let symbol_str = data.trim_start_matches("unmute_");
    let Ok(symbol) = Symbol::from_str(symbol_str) else {
        bot.answer_callback_query(q.id.clone()).await?;
        return Ok(());
    };

    match deps.storage.unmute_symbol(user_id, symbol).await {
        Ok(()) => {
            bot.answer_callback_query(q.id.clone())
                .text("🔊 已取消静音，您将重新接收该交易对的信号。")
                .await?;

            let new_keyboard = build_keyboard_for_user(&deps.storage, user_id, symbol).await;
            bot.edit_message_reply_markup(chat_id, msg_id)
                .reply_markup(new_keyboard)
                .await?;
        }
        Err(e) => {
            bot.answer_callback_query(q.id.clone())
                .text(format!("操作失败: {}", e))
                .show_alert(true)
                .await?;
        }
    }
    Ok(())
}

async fn handle_close(
    bot: &Bot,
    q: &CallbackQuery,
    data: &str,
    chat_id: ChatId,
    msg_id: MessageId,
    deps: &CallbackDeps,
    user_id: i64,
) -> Result<()> {
    let symbol_str = data.trim_start_matches("close_");
    let Ok(symbol) = Symbol::from_str(symbol_str) else {
        bot.answer_callback_query(q.id.clone()).await?;
        return Ok(());
    };

    match deps
        .storage
        .close_trade(user_id, symbol, "manual_close")
        .await
    {
        Ok(()) => {
            bot.answer_callback_query(q.id.clone())
                .text("仓位已关闭")
                .await?;

            let new_keyboard = build_keyboard_for_user(&deps.storage, user_id, symbol).await;
            bot.edit_message_reply_markup(chat_id, msg_id)
                .reply_markup(new_keyboard)
                .await?;
        }
        Err(e) => {
            bot.answer_callback_query(q.id.clone())
                .text(format!("关闭失败: {}", e))
                .show_alert(true)
                .await?;
        }
    }
    Ok(())
}

async fn handle_exec(
    bot: &Bot,
    q: &CallbackQuery,
    data: &str,
    chat_id: ChatId,
    msg_id: MessageId,
    deps: &CallbackDeps,
    user_id: i64,
) -> Result<()> {
    let symbol_str = data.trim_start_matches("exec_");
    let Ok(symbol) = Symbol::from_str(symbol_str) else {
        bot.answer_callback_query(q.id.clone()).await?;
        return Ok(());
    };

    if deps.storage.has_open_trade(user_id, symbol).await? {
        bot.answer_callback_query(q.id.clone())
            .text("您已有未平仓仓位，请先关闭。")
            .show_alert(true)
            .await?;
        return Ok(());
    }

    bot.answer_callback_query(q.id.clone())
        .text("执行信号功能将在下个版本中通过实时分析触发，敬请期待。")
        .show_alert(true)
        .await?;
    Ok(())
}
