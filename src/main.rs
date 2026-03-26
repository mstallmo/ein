mod bindings;
mod syscalls;
mod tools;

use anyhow::anyhow;
use async_openai::{Client, config::OpenAIConfig};
use clap::Parser;
use ein_tool::Role;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{env, path::PathBuf, process};
use wasmtime::Engine;
use wasmtime::component::*;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use crate::bindings::Plugin;
use crate::tools::ToolRegistry;

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

struct HarnessState {
    resource_table: ResourceTable,
    wasi_ctx: WasiCtx,
}

impl WasiView for HarnessState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.resource_table,
        }
    }
}


#[derive(Debug, Clone)]
struct EinConfig {
    #[expect(unused)]
    ein_dir: PathBuf,
    plugin_dir: PathBuf,
}

impl Default for EinConfig {
    fn default() -> Self {
        let ein_dir = dirs::home_dir()
            .expect("Failed to load EinConfig, Missing home directory")
            .join(".ein");

        let plugin_dir = if cfg!(debug_assertions) {
            PathBuf::from("./target/wasm32-wasip2/debug")
        } else {
            ein_dir.join("plugins")
        };

        Self {
            ein_dir,
            plugin_dir,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let ein_config = EinConfig::default();

    let engine = Engine::default();
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    Plugin::add_to_linker::<HarnessState, HasSelf<HarnessState>>(&mut linker, |state| state)?;

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

    let mut tool_registry = ToolRegistry::load(&engine, &linker, &ein_config.plugin_dir).await?;

    loop {
        let response: Value = client
            .chat()
            .create_byot(json!({
                "messages": messages,
                "model": "anthropic/claude-haiku-4.5",
                "tools": tool_registry.schemas()?,
                "max_tokens": 2500,
            }))
            .await?;

        // Uncomment for debug information
        eprintln!("{:#?}", response);

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

                        let res = tool.call(id, &function.arguments).await?;
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
