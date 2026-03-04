use super::prelude::*;
use serde::Deserialize;
use std::process;

#[derive(Debug, Clone, Deserialize)]
struct BashArgs {
    command: String,
}

#[derive(Debug, Clone)]
pub struct BashTool;

impl Tool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }

    fn schema(&self) -> ToolDef {
        ToolDef::function(self.name(), "Execute a shell command")
            .param("command", "string", "The command to execute", true)
            .build()
    }

    fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult> {
        let args: BashArgs = serde_json::from_str(args)?;

        let output = process::Command::new("sh")
            .args(["-c", &args.command])
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let message = format!(
            "Exit code: {}\nStdout:\n{stdout}\nStderr:\n{stderr}",
            output.status.code().unwrap_or(-1)
        );

        Ok(ToolResult::new(id, message))
    }
}
