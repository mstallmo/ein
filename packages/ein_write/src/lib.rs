use ein_plugin::{ConstructableToolPlugin, ToolDef, ToolPlugin, ToolResult};
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
        ToolDef::function(self.name(), "Write content to a file")
            .param(
                "file_path",
                "string",
                "The path of the file to write to",
                true,
            )
            .param(
                "content",
                "string",
                "The content to write to the file",
                true,
            )
            .build()
    }

    fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult> {
        let args: WriteArgs = serde_json::from_str(args)?;

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

ein_plugin::export!(WriteTool);
