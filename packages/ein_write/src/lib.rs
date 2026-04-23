// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use ein_plugin::tool::{ConstructableToolPlugin, ToolDef, ToolPlugin, ToolResult};
use serde::Deserialize;
use std::{fs, io::Write};

#[derive(Debug, Clone, Deserialize)]
struct WriteArgs {
    file_path: String,
    content: String,
}

#[derive(Debug, Clone)]
pub struct WriteTool;

impl ConstructableToolPlugin for WriteTool {
    fn new() -> Self {
        Self {}
    }
}

impl ToolPlugin for WriteTool {
    fn name(&self) -> &str {
        "Write"
    }

    fn schema(&self) -> ToolDef {
        ToolDef::function(
            self.name(),
            "Write content to a file, creating it and any missing parent directories. \
             Pass the complete intended file contents as `content` — do not truncate or \
             summarise. Call this tool directly; there is no need to create directories \
             beforehand.",
        )
        .param(
            "file_path",
            "string",
            "The path of the file to write to",
            true,
        )
        .param(
            "content",
            "string",
            "The complete content to write to the file",
            true,
        )
        .build()
    }

    fn primary_arg(&self) -> Option<&str> {
        Some("file_path")
    }

    fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult> {
        let args: WriteArgs = serde_json::from_str(args)?;

        if let Some(parent) = std::path::Path::new(&args.file_path).parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }

        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&args.file_path)?;
        file.write_all(args.content.as_bytes())?;

        Ok(ToolResult::new(
            id,
            format!(
                "Successfully wrote {} bytes to {}",
                args.content.len(),
                args.file_path
            ),
        ))
    }
}

#[cfg(target_arch = "wasm32")]
ein_plugin::export_tool!(WriteTool);
