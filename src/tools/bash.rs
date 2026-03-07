use super::Tool;
use anyhow::Context;
use async_trait::async_trait;
use ein_plugin::{ToolDef, ToolResult};
use serde::Deserialize;
use std::process;

#[derive(Debug, Clone, Deserialize)]
struct BashArgs {
    command: String,
}

#[derive(Debug, Clone)]
pub struct BashTool {
    name: String,
    def: ToolDef,
}

impl BashTool {
    pub(crate) fn new() -> Self {
        let name = "Bash".to_string();
        let def = ToolDef::function(&name, "Execute a shell command")
            .param("command", "string", "The command to execute", true)
            .build();

        Self { name, def }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> &ToolDef {
        &self.def
    }

    async fn call(&mut self, id: &str, args: &str) -> anyhow::Result<ToolResult> {
        let args = args.to_owned();
        let message = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let args: BashArgs = serde_json::from_str(&args)?;

            let output = process::Command::new("sh")
                .args(["-c", &args.command])
                .output()
                .with_context(|| "Failed to create `sh` command")?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            let message = format!(
                "Exit code: {}\nStdout:\n{stdout}\nStderr:\n{stderr}",
                output.status.code().unwrap_or(-1)
            );

            Ok(message)
        })
        .await??;

        Ok(ToolResult::new(id, message))
    }
}
