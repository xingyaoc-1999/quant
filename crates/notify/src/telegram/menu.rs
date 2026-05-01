use common::Symbol;
use storage::postgres::Storage;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

pub(crate) async fn build_keyboard_for_user(
    storage: &Storage,
    user_id: i64,
    symbol: Symbol,
) -> InlineKeyboardMarkup {
    let has_open = storage
        .has_open_trade(user_id, symbol)
        .await
        .unwrap_or(false);

    let is_muted = storage.is_muted(user_id, symbol).await.unwrap_or(false);

    let action_btn = if has_open {
        InlineKeyboardButton::callback("🔒 关闭仓位", format!("close_{}", symbol))
    } else {
        InlineKeyboardButton::callback("📈 执行信号", format!("exec_{}", symbol))
    };

    let mute_btn = if is_muted {
        InlineKeyboardButton::callback("🔊 取消静音", format!("unmute_{}", symbol))
    } else {
        InlineKeyboardButton::callback("🔕 静音", format!("mute_{}", symbol))
    };

    InlineKeyboardMarkup::new(vec![
        vec![action_btn],
        vec![InlineKeyboardButton::callback(
            "🔍 AI 审计",
            format!("audit_{}", symbol),
        )],
        vec![mute_btn],
    ])
}
