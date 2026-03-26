use std::sync::Arc;
use std::{env, process};

use async_openai::{Client, config::OpenAIConfig};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use wasmtime::Engine;
use wasmtime::component::{HasSelf, Linker};

use crate::HarnessState;
use crate::bindings::Plugin;
use crate::tools::ToolRegistry;
use crate::agent::run_agent;
use ein_proto::ein::{AgentEvent, RunAgentRequest, agent_server::Agent};

pub struct AgentServer {
    engine: Arc<Engine>,
    linker: Arc<Linker<HarnessState>>,
    config: Arc<crate::EinConfig>,
    client: Arc<Client<OpenAIConfig>>,
}

impl AgentServer {
    pub fn new() -> anyhow::Result<Self> {
        let engine = Engine::default();
        let mut linker: Linker<HarnessState> = Linker::new(&engine);
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

        Ok(Self {
            engine: Arc::new(engine),
            linker: Arc::new(linker),
            config: Arc::new(crate::EinConfig::default()),
            client: Arc::new(client),
        })
    }
}

#[tonic::async_trait]
impl Agent for AgentServer {
    type RunAgentStream = ReceiverStream<Result<AgentEvent, Status>>;

    async fn run_agent(
        &self,
        request: Request<RunAgentRequest>,
    ) -> Result<Response<Self::RunAgentStream>, Status> {
        let prompt = request.into_inner().prompt;
        let (tx, rx) = mpsc::channel(32);

        let engine = self.engine.clone();
        let linker = self.linker.clone();
        let config = self.config.clone();
        let client = self.client.clone();

        tokio::spawn(async move {
            let registry = ToolRegistry::load(&engine, &linker, &config.plugin_dir).await;
            match registry {
                Ok(mut registry) => {
                    if let Err(e) = run_agent(prompt, &mut registry, &client, tx.clone()).await {
                        let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                    }
                }
                Err(e) => {
                    let _ = tx
                        .send(Err(Status::internal(format!(
                            "Failed to load plugins: {e}"
                        ))))
                        .await;
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}
