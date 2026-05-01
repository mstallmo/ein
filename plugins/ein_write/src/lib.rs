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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tool() -> WriteTool {
        WriteTool::new()
    }

    fn call(path: &str, content: &str) -> anyhow::Result<ToolResult> {
        let args = serde_json::json!({ "file_path": path, "content": content }).to_string();
        tool().call("id", &args)
    }

    #[test]
    fn write_creates_file_with_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.txt");
        call(path.to_str().unwrap(), "hello world").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello world");
    }

    #[test]
    fn write_overwrites_existing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.txt");
        call(path.to_str().unwrap(), "first").unwrap();
        call(path.to_str().unwrap(), "second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    }

    #[test]
    fn write_creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a").join("b").join("c").join("file.txt");
        call(path.to_str().unwrap(), "nested").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "nested");
    }

    #[test]
    fn write_empty_content_creates_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.txt");
        call(path.to_str().unwrap(), "").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn write_returns_success_message_with_byte_count_and_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("f.txt");
        let result = call(path.to_str().unwrap(), "abc").unwrap();
        assert!(
            result.content.contains('3'),
            "expected byte count in: {}",
            result.content
        );
        assert!(
            result.content.contains(path.to_str().unwrap()),
            "expected path in: {}",
            result.content
        );
    }
}
