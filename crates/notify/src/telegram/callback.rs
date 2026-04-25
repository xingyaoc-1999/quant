// callback.rs
use anyhow::Result;
use common::Symbol;
use quant::{analyzer::AnalysisEngine, config::AnalyzerConfig, risk_manager::RiskAssessment};
use service::integrity::context::FeatureContextManager;
use std::str::FromStr;
use std::sync::Arc;
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

    match data {
        d if d.starts_with("exec_") => handle_exec(&bot, &q, d, chat_id, msg_id, &deps).await?,
        d if d.starts_with("mute_") => handle_mute(&bot, &q, d, chat_id, msg_id, &deps).await?,
        _ => {}
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
) -> Result<()> {
    let symbol_str = data.trim_start_matches("mute_");
    let Ok(symbol) = Symbol::from_str(symbol_str) else {
        bot.answer_callback_query(q.id.clone()).await?;
        return Ok(());
    };

    match deps.storage.mute_symbol(chat_id.0, symbol).await {
        Ok(()) => {
            bot.answer_callback_query(q.id.clone()).await?;
            bot.edit_message_text(
                chat_id,
                msg_id,
                format!("🔕 已静音 {}，不再接收该交易对的信号。", symbol),
            )
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

async fn handle_exec(
    _bot: &Bot,
    _q: &CallbackQuery,
    _data: &str,
    _chat_id: ChatId,
    _msg_id: MessageId,
    _deps: &CallbackDeps,
) -> Result<()> {
    // TODO: 实现执行逻辑
    Ok(())
}
