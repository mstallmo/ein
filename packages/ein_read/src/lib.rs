// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use ein_plugin::tool::{ConstructableToolPlugin, ToolDef, ToolPlugin, ToolResult};
use serde::Deserialize;
use std::fs;

fn default_limit() -> usize {
    200
}

#[derive(Debug, Clone, Deserialize)]
struct ReadArgs {
    file_path: String,
    /// Line number to start reading from (0-indexed). Defaults to 0.
    #[serde(default)]
    offset: usize,
    /// Maximum number of lines to return. Defaults to 200.
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(Debug, Clone)]
pub struct ReadTool;

impl ConstructableToolPlugin for ReadTool {
    fn new() -> Self {
        Self {}
    }
}

impl ToolPlugin for ReadTool {
    fn name(&self) -> &str {
        "Read"
    }

    fn schema(&self) -> ToolDef {
        ToolDef::function(
            self.name(),
            "Read and return the contents of a file. Returns up to `limit` lines starting at \
             `offset`. When a file is truncated a header shows the line range and total line \
             count — use `offset` to paginate and read further sections.",
        )
        .param("file_path", "string", "The path to the file to read", true)
        .param(
            "offset",
            "integer",
            "Line number to start reading from (0-indexed, default 0)",
            false,
        )
        .param(
            "limit",
            "integer",
            "Maximum number of lines to return (default 200)",
            false,
        )
        .build()
    }

    fn primary_arg(&self) -> Option<&str> {
        Some("file_path")
    }

    fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult> {
        ein_plugin::tool::syscalls::log(&format!("Reading file with args: {args}"));

        let args: ReadArgs = serde_json::from_str(args)?;

        let raw = fs::read_to_string(&args.file_path)?;
        let all_lines: Vec<&str> = raw.lines().collect();
        let total = all_lines.len();

        let start = args.offset.min(total);
        let end = (start + args.limit).min(total);
        let window = &all_lines[start..end];

        let content = if end < total {
            // File was truncated — prepend a header so the model knows.
            format!(
                "Lines {}-{} of {} (use offset={} to read more):\n{}",
                start + 1,
                end,
                total,
                end,
                window.join("\n"),
            )
        } else {
            window.join("\n")
        };

        Ok(ToolResult::new(id, content))
    }
}

#[cfg(target_arch = "wasm32")]
ein_plugin::export_tool!(ReadTool);
