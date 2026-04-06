use quant::report::AnalysisReport;
use rig::{
    client::CompletionClient,
    completion::{CompletionRequestBuilder, ToolDefinition},
    message::{AssistantContent, Message, ToolChoice, ToolResultContent, UserContent},
    providers::{gemini, openai},
    tool::ToolSet,
    OneOrMany,
};

use anyhow::{anyhow, Context, Result};
use schemars::Schema;
use tracing::info;

pub mod prompt;
pub mod technical;

pub enum Model {
    OpenAI(openai::responses_api::ResponsesCompletionModel),
    Gemini(gemini::CompletionModel),
}

impl Model {
    pub fn openai(api_key: &str, base_url: &str, model: impl Into<String>) -> Result<Self> {
        let provider = rig::providers::openai::Client::builder()
            .api_key(api_key)
            .base_url(base_url)
            .build()?;
        let completion_model = provider.completion_model(model);
        Ok(Self::OpenAI(completion_model))
    }

    pub fn gemini(api_key: &str, base_url: &str, model: impl Into<String>) -> Result<Self> {
        let provider = rig::providers::gemini::Client::builder()
            .api_key(api_key)
            .base_url(base_url)
            .build()?;
        let completion_model = provider.completion_model(model);
        Ok(Self::Gemini(completion_model))
    }
    async fn request_completion<M: rig::completion::CompletionModel>(
        &self,
        model: M,
        current_msg: Message,
        history: Vec<Message>,
        preamble: String,
        definitions: Vec<ToolDefinition>,
        schema: Schema,
    ) -> Result<OneOrMany<AssistantContent>> {
        let response = CompletionRequestBuilder::new(model, current_msg)
            .preamble(preamble)
            .messages(history)
            .temperature(0.2)
            .tools(definitions)
            .tool_choice(ToolChoice::Auto)
            .output_schema(schema)
            .send()
            .await
            .context("Failed to get model response")?;

        Ok(response.choice)
    }
    pub async fn complete(
        &self,
        prompt: &str,
        preamble: String,
        tool_set: &ToolSet,
        chat_history: &mut Vec<Message>,
        max_turns: usize,
    ) -> Result<String> {
        chat_history.push(Message::User {
            content: OneOrMany::one(UserContent::text(prompt)),
        });

        let definitions = tool_set.get_tool_definitions().await?;
        let mut turns = 0;
        let schema = schemars::schema_for!(AnalysisReport);

        while turns < max_turns {
            let (current_msg, history) = chat_history
                .split_last()
                .ok_or_else(|| anyhow!("Chat history is empty"))?;

            let choices = match self {
                Model::OpenAI(model) => {
                    self.request_completion(
                        model.clone(),
                        current_msg.clone(),
                        history.to_vec(),
                        preamble.clone(),
                        definitions.clone(),
                        schema.clone(),
                    )
                    .await?
                }
                Model::Gemini(model) => {
                    self.request_completion(
                        model.clone(),
                        current_msg.clone(),
                        history.to_vec(),
                        preamble.clone(),
                        definitions.clone(),
                        schema.clone(),
                    )
                    .await?
                }
            };

            let mut tool_calls = Vec::new();
            let mut assistant_contents = Vec::new();
            let mut current_turn_text = String::new();

            for content in choices.iter() {
                assistant_contents.push(content.clone());
                match content {
                    AssistantContent::ToolCall(tc) => tool_calls.push(tc.clone()),
                    AssistantContent::Text(t) => current_turn_text.push_str(&t.text),
                    _ => {}
                }
            }

            chat_history.push(Message::Assistant {
                id: None,
                content: OneOrMany::many(assistant_contents)
                    .map_err(|_| anyhow!("Content error"))?,
            });

            if tool_calls.is_empty() {
                chat_history.clear();
                // chat_history.retain(|msg| match msg {
                //     Message::User { content } => {
                //         content.iter().any(|c| matches!(c, UserContent::Text(_)))
                //     }
                //     Message::Assistant { content, .. } => content
                //         .iter()
                //         .any(|c| matches!(c, AssistantContent::Text(_))),
                //     Message::System { content } => {true}
                // });

                return Ok(current_turn_text);
            }

            let mut results = Vec::new();
            for tool_call in tool_calls {
                info!(?tool_call);

                let result = tool_set
                    .call(
                        &tool_call.function.name,
                        serde_json::to_string(&tool_call.function.arguments)?,
                    )
                    .await?;

                let tool_res_content = OneOrMany::one(ToolResultContent::text(result));
                let user_content = tool_call
                    .call_id
                    .as_ref()
                    .map(|call_id| {
                        UserContent::tool_result_with_call_id(
                            tool_call.id.clone(),
                            call_id.clone(),
                            tool_res_content.clone(),
                        )
                    })
                    .unwrap_or_else(|| UserContent::tool_result(tool_call.id, tool_res_content));

                results.push(user_content);
            }

            chat_history.push(Message::User {
                content: OneOrMany::many(results)
                    .map_err(|_| anyhow!("Failed to bundle tool results"))?,
            });

            turns += 1;

            if turns == max_turns {
                return Err(anyhow!("Reached max turns without final analysis report"));
            }
        }

        Err(anyhow!("Unexpected loop exit"))
    }
}
