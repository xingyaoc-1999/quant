use anyhow::Result;
use common::Symbol;
use quant::{analyzer::AnalysisEngine, config::AnalyzerConfig, risk_manager::RiskAssessment};
use service::integrity::context::FeatureContextManager;
use std::{str::FromStr, sync::Arc};
use storage::postgres::Storage;
use teloxide::{
    prelude::Requester,
    types::{CallbackQuery, ChatId, MessageId},
    Bot,
};

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
        bot.answer_callback_query(q.id).await?;
        return Ok(());
    };
    deps.storage.mute_symbol(chat_id.0, symbol).await?;
    bot.answer_callback_query(q.id).await?;
    bot.edit_message_text(
        chat_id,
        msg_id,
        format!("🔕 已静音 {}，不再接收该交易对的信号。", symbol),
    )
    .await?;

    Ok(())
}

async fn handle_exec(
    bot: &Bot,
    q: &CallbackQuery,
    data: &str,
    chat_id: ChatId,
    msg_id: MessageId,
    deps: &CallbackDeps,
) -> Result<()> {
    Ok(())
}
