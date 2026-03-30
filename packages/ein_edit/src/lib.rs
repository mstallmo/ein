use ein_plugin::tool::{ConstructableToolPlugin, ToolDef, ToolPlugin, ToolResult};
use serde::Deserialize;
use serde_json::json;
use std::fs;

#[derive(Debug, Clone, Deserialize)]
struct EditArgs {
    file_path: String,
    old_string: String,
    new_string: String,
}

#[derive(Debug, Clone)]
pub struct EditTool;

impl ConstructableToolPlugin for EditTool {
    fn new() -> Self {
        Self {}
    }
}

impl ToolPlugin for EditTool {
    fn name(&self) -> &str {
        "Edit"
    }

    fn schema(&self) -> ToolDef {
        ToolDef::function(
            self.name(),
            "Replace a specific string in a file with new content",
        )
        .param("file_path", "string", "The path to the file to edit", true)
        .param("old_string", "string", "The exact string to replace", true)
        .param(
            "new_string",
            "string",
            "The string to replace it with",
            true,
        )
        .build()
    }

    fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult> {
        let args: EditArgs = serde_json::from_str(args)?;

        let content = fs::read_to_string(&args.file_path)?;

        let match_pos = content
            .find(&args.old_string)
            .ok_or_else(|| anyhow::anyhow!("old_string not found in {}", args.file_path))?;

        // Count newlines before the match to get a 1-based start line.
        let start_line = content[..match_pos].bytes().filter(|&b| b == b'\n').count() as u32 + 1;

        let new_content = content.replacen(&args.old_string, &args.new_string, 1);
        fs::write(&args.file_path, &new_content)?;

        let metadata = json!({
            "start_line": start_line,
            "old_lines": args.old_string.lines().collect::<Vec<_>>(),
            "new_lines": args.new_string.lines().collect::<Vec<_>>(),
        });

        Ok(ToolResult::with_metadata(
            id,
            format!("Successfully edited {}", args.file_path),
            metadata,
        ))
    }
}

ein_plugin::export_tool!(EditTool);
