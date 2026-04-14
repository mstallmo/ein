// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use anyhow::anyhow;
use ein_plugin::tool::{ConstructableToolPlugin, ToolDef, ToolPlugin, ToolResult};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct BashTool {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct BashArgs {
    command: String,
}

impl ConstructableToolPlugin for BashTool {
    fn new() -> Self {
        let name = "Bash".to_string();

        Self { name }
    }
}

impl ToolPlugin for BashTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> ToolDef {
        ToolDef::function(self.name(), "Execute a shell command")
            .param("command", "string", "The command to execute", true)
            .build()
    }

    fn enable_chunk_sender(&self) -> bool {
        true
    }

    fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult> {
        let args: BashArgs = serde_json::from_str(args)?;

        let result =
            ein_plugin::tool::syscalls::spawn(&args.command).map_err(|err| anyhow!(err))?;

        let content = if result.is_empty() {
            "(no output)".to_string()
        } else {
            result
        };

        Ok(ToolResult::new(id, content))
    }
}

ein_plugin::export_tool!(BashTool);
