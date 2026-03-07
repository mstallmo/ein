use ein_plugin::{ConstructableToolPlugin, ToolDef, ToolPlugin, ToolResult};
use serde::Deserialize;
use std::fs;

#[derive(Debug, Clone, Deserialize)]
struct ReadArgs {
    file_path: String,
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
        ToolDef::function(self.name(), "Read and return the contents of a file")
            .param("file_path", "string", "The path to the file to read", true)
            .build()
    }

    fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult> {
        let args: ReadArgs = serde_json::from_str(args)?;

        let file_contents = fs::read_to_string(&args.file_path)?;
        Ok(ToolResult::new(id, file_contents))
    }
}

ein_plugin::export!(ReadTool);
