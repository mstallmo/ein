// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

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

    fn primary_arg(&self) -> Option<&str> {
        Some("file_path")
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

#[cfg(target_arch = "wasm32")]
ein_plugin::export_tool!(EditTool);

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn tool() -> EditTool {
        EditTool::new()
    }

    fn call(path: &str, old: &str, new: &str) -> anyhow::Result<ToolResult> {
        let args = serde_json::json!({
            "file_path": path,
            "old_string": old,
            "new_string": new,
        })
        .to_string();
        tool().call("id", &args)
    }

    fn write_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    // ---------------------------------------------------------------------------
    // Replacement behaviour
    // ---------------------------------------------------------------------------

    #[test]
    fn edit_replaces_first_occurrence_only() {
        let f = write_temp("hello world world");
        let result = call(f.path().to_str().unwrap(), "world", "Rust").unwrap();
        let on_disk = fs::read_to_string(f.path()).unwrap();
        assert_eq!(on_disk, "hello Rust world");
        assert!(result.content.contains("Successfully edited"));
    }

    #[test]
    fn edit_empty_new_string_deletes_matched_text() {
        let f = write_temp("remove_me keep");
        call(f.path().to_str().unwrap(), "remove_me ", "").unwrap();
        assert_eq!(fs::read_to_string(f.path()).unwrap(), "keep");
    }

    #[test]
    fn edit_multiline_old_string_replaced_correctly() {
        let f = write_temp("line1\nline2\nline3\n");
        call(f.path().to_str().unwrap(), "line1\nline2", "replaced").unwrap();
        assert_eq!(fs::read_to_string(f.path()).unwrap(), "replaced\nline3\n");
    }

    // ---------------------------------------------------------------------------
    // Metadata: start_line
    // ---------------------------------------------------------------------------

    #[test]
    fn edit_start_line_is_1_for_match_at_top() {
        let f = write_temp("target here\nother line\n");
        let result = call(f.path().to_str().unwrap(), "target", "X").unwrap();
        let meta = result.metadata.unwrap();
        assert_eq!(meta["start_line"], 1);
    }

    #[test]
    fn edit_start_line_accounts_for_preceding_newlines() {
        let f = write_temp("line1\nline2\ntarget\nline4\n");
        let result = call(f.path().to_str().unwrap(), "target", "X").unwrap();
        let meta = result.metadata.unwrap();
        assert_eq!(meta["start_line"], 3);
    }

    #[test]
    fn edit_multiline_start_line_is_line_of_first_character() {
        let f = write_temp("a\nb\nc\nd\n");
        // old_string starts at line 2
        let result = call(f.path().to_str().unwrap(), "b\nc", "X").unwrap();
        let meta = result.metadata.unwrap();
        assert_eq!(meta["start_line"], 2);
    }

    // ---------------------------------------------------------------------------
    // Metadata: old_lines / new_lines
    // ---------------------------------------------------------------------------

    #[test]
    fn edit_metadata_contains_old_and_new_lines() {
        let f = write_temp("foo\nbar\nbaz\n");
        let result = call(f.path().to_str().unwrap(), "bar", "qux").unwrap();
        let meta = result.metadata.unwrap();
        assert_eq!(meta["old_lines"], serde_json::json!(["bar"]));
        assert_eq!(meta["new_lines"], serde_json::json!(["qux"]));
    }

    #[test]
    fn edit_metadata_multiline_old_and_new_lines() {
        let f = write_temp("a\nb\nc\n");
        let result = call(f.path().to_str().unwrap(), "a\nb", "x\ny\nz").unwrap();
        let meta = result.metadata.unwrap();
        assert_eq!(meta["old_lines"], serde_json::json!(["a", "b"]));
        assert_eq!(meta["new_lines"], serde_json::json!(["x", "y", "z"]));
    }

    // ---------------------------------------------------------------------------
    // Error cases
    // ---------------------------------------------------------------------------

    #[test]
    fn edit_returns_error_when_old_string_not_found() {
        let f = write_temp("hello world");
        let err = call(f.path().to_str().unwrap(), "no such string", "x").unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    #[test]
    fn edit_returns_error_for_missing_file() {
        let err = call("/nonexistent/path/file.txt", "x", "y").unwrap_err();
        assert!(err.to_string().contains("No such file") || err.to_string().contains("os error"));
    }
}
