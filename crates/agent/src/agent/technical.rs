use anyhow::Result;
use common::Symbol;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use rig::message::Message;
use rig::tool::ToolSet;
use teloxide::types::ChatId;

use tokio::sync::mpsc::Sender;
use tracing::{debug, error, info};

use crate::agent::{prompt::ANALYSIS_PROMPT_RUST, Model};

#[derive(Debug, Clone)]
pub enum TechnicalAgentMessage {
    Task(String, ChatId),
    Shutdown,
}

pub struct TechnicalAgent;

pub struct TechnicalAgentState {
    conversation_history: Vec<Message>,
    model: Model,
    tx_out: Sender<(String, Symbol)>,
    tool_set: ToolSet,
}

impl TechnicalAgentState {
    const MAX_TURNS: usize = 12;
    pub async fn process_task(&mut self, task: &str) -> Result<String> {
        let response = self
            .model
            .complete(
                task,
                ANALYSIS_PROMPT_RUST.to_owned(),
                &self.tool_set,
                &mut self.conversation_history,
                Self::MAX_TURNS,
            )
            .await;
        response
    }
}

pub struct TechnicalAgentArgs {
    pub model: Model,
    pub tx_out: Sender<(String, Symbol)>,
    pub tool_set: ToolSet,
}

impl Actor for TechnicalAgent {
    type Msg = TechnicalAgentMessage;
    type State = TechnicalAgentState;
    type Arguments = TechnicalAgentArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let actor_name = myself
            .get_name()
            .unwrap_or_else(|| "Anonymous ".to_string());

        info!("Agent {} starting...", actor_name);

        let conversation_history: Vec<Message> = Vec::new();
        Ok(TechnicalAgentState {
            model: args.model,
            conversation_history,
            tx_out: args.tx_out,
            tool_set: args.tool_set,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            TechnicalAgentMessage::Task(task, chat_id) => {
                let actor_name = myself
                    .get_name()
                    .unwrap_or_else(|| "Anonymous ".to_string());
                debug!("[{}] Received task: {}", actor_name, task);
                // match state.process_task(&task).await {
                //     Ok(output) => {
                //         if let Err(e) = state.tx_out.send((output, chat_id)).await {
                //             error!("[{}] Failed to send response: {:?}", actor_name, e);
                //         }
                //     }
                //     Err(e) => {
                //         error!("[{}] Task processing error: {:?}", actor_name, e);
                //         let error_msg = format!("处理失败: {}", e);
                //         if let Err(e) = state.tx_out.send((error_msg, chat_id)).await {
                //             error!("[{}] Failed to send response: {}", actor_name, e);
                //         }
                //     }
                // };
                Ok(())
            }
            TechnicalAgentMessage::Shutdown => {
                let actor_name = myself
                    .get_name()
                    .unwrap_or_else(|| "Anonymous ".to_string());
                info!("[{}] Shutting down...", actor_name);
                Ok(())
            }
        }
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let actor_name = myself
            .get_name()
            .unwrap_or_else(|| "Anonymous ".to_string());
        info!("Agent {} stopped", actor_name);
        Ok(())
    }
}
