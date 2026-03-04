mod tools;

use crate::tools::{BashTool, ReadTool, Role, Tool, WriteTool};
use anyhow::anyhow;
use async_openai::{Client, config::OpenAIConfig};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections, env, process};

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Choice {
    index: usize,
    finish_reason: FinishReason,
    message: LlmMessage,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum FinishReason {
    Stop,
    ToolCalls,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LlmMessage {
    role: Role,
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
enum ToolCall {
    Function {
        id: String,
        index: usize,
        function: FunctionCall,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct FunctionCall {
    name: String,
    arguments: String,
}

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    #[arg(short = 'p', long)]
    prompt: String,
}

struct ToolRegistry(collections::HashMap<String, Box<dyn Tool>>);

impl ToolRegistry {
    fn new() -> Self {
        Self(collections::HashMap::<String, Box<dyn Tool>>::new())
    }

    fn add_tool(&mut self, tool: impl Tool + 'static) {
        self.0.insert(tool.name().to_string(), Box::new(tool));
    }

    fn schemas(&self) -> Result<Vec<Value>, serde_json::Error> {
        self.0
            .values()
            .map(|tool| serde_json::to_value(tool.schema()))
            .collect::<Result<Vec<_>, serde_json::Error>>()
    }

    fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.0.get(name).map(|b| b.as_ref())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let base_url = env::var("OPENROUTER_BASE_URL")
        .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string());

    let api_key = env::var("OPENROUTER_API_KEY").unwrap_or_else(|_| {
        eprintln!("OPENROUTER_API_KEY is not set");
        process::exit(1);
    });

    let config = OpenAIConfig::new()
        .with_api_base(base_url)
        .with_api_key(api_key);

    let client = Client::with_config(config);

    let mut messages = vec![json!({
            "role": "user",
            "content": args.prompt
    })];

    let mut tool_registry = ToolRegistry::new();
    tool_registry.add_tool(ReadTool {});
    tool_registry.add_tool(WriteTool {});
    tool_registry.add_tool(BashTool {});

    loop {
        let response: Value = client
            .chat()
            .create_byot(json!({
                "messages": messages,
                "model": "anthropic/claude-haiku-4.5",
                "tools": tool_registry.schemas()?,
                "max_tokens": 5000,
            }))
            .await?;

        // Uncomment for debug information
        // eprintln!("{:#?}", response);

        let choices: Vec<Choice> = response
            .get("choices")
            .map(|v| serde_json::from_value(v.clone()))
            .ok_or_else(|| anyhow!("Response missing 'choices' field"))??;
        let choice = choices
            .first()
            .ok_or_else(|| anyhow!("Response contained no choices"))?;

        messages.push(serde_json::to_value(choice.message.clone())?);

        let content = choice.message.content.as_deref().unwrap_or_default();

        if let Some(tool_calls) = &choice.message.tool_calls {
            for tool_call in tool_calls {
                match tool_call {
                    ToolCall::Function { id, function, .. } => {
                        let Some(tool) = tool_registry.get(function.name.as_str()) else {
                            return Err(anyhow!("Missing tool {}", function.name));
                        };

                        let res = tool.call(id, &function.arguments)?;
                        messages.push(serde_json::to_value(res)?);
                    }
                };
            }

            eprintln!("Agent: {content}",);
        } else {
            println!("{content}");
        }

        if matches!(choice.finish_reason, FinishReason::Stop) {
            break;
        }
    }

    Ok(())
}
