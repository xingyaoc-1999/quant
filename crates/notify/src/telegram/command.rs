use crate::telegram::BotApp;
use service::context::FeatureContextManager;
use std::sync::Arc;
use teloxide::utils::command::BotCommands;
use teloxide::{prelude::*, types::BotCommand};
use tokio::sync::mpsc::Sender;
use tracing::error;

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase")]
pub enum MyCommand {
    Ai(String),
    Start,
    Help,
    #[command(description = "查看/快捷修改现有配置")]
    List,
}

impl MyCommand {
    pub async fn handle(
        self,
        bot: &Bot,
        chat_id: ChatId,
        sender: Sender<(String, ChatId)>,
        manager: Arc<FeatureContextManager>,
    ) -> ResponseResult<()> {
        match self {
            MyCommand::Ai(args) => {
                if args.is_empty() {
                    bot.send_message(chat_id, "请在 /ai 后输入文字。").await?;
                } else {
                    // 保留你原来的异步发送逻辑
                    if let Err(err) = sender.send((args.clone(), chat_id)).await {
                        error!("Channel send error: {:?}", err);
                    }
                    bot.send_message(chat_id, format!("🤖 AI 正在分析：{}...", args))
                        .await?;
                }
            }
            MyCommand::Start => {
                bot.send_message(chat_id, "✅ 交易助手已就绪。\n/add <币种> - 新增\n/list - 查看与快速修改\n/ai - 智能分析").await?;
            }
            MyCommand::Help => {
                bot.send_message(chat_id, "命令说明：\n/list - 显示所有已配置的币种，点击按钮直接修改周期\n/add BTCUSDT - 直接为 BTCUSDT 设置新角色\n/ai <文字> - 提交分析请求").await?;
            }
            MyCommand::List => {
                BotApp::send_config_list(bot, chat_id, manager).await?;
            }
        }
        Ok(())
    }
}
