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
        #[cfg(target_arch = "wasm32")]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn tool() -> ReadTool {
        ReadTool::new()
    }

    fn call(path: &str, offset: Option<usize>, limit: Option<usize>) -> anyhow::Result<String> {
        let mut args = serde_json::json!({ "file_path": path });
        if let Some(o) = offset {
            args["offset"] = serde_json::json!(o);
        }
        if let Some(l) = limit {
            args["limit"] = serde_json::json!(l);
        }
        tool().call("id", &args.to_string()).map(|r| r.content)
    }

    fn write_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    // ---------------------------------------------------------------------------
    // Basic reading
    // ---------------------------------------------------------------------------

    #[test]
    fn read_returns_all_lines_by_default() {
        let f = write_temp("a\nb\nc\nd\ne\n");
        let out = call(f.path().to_str().unwrap(), None, None).unwrap();
        assert_eq!(out, "a\nb\nc\nd\ne");
    }

    #[test]
    fn read_empty_file_returns_empty_string() {
        let f = write_temp("");
        let out = call(f.path().to_str().unwrap(), None, None).unwrap();
        assert_eq!(out, "");
    }

    // ---------------------------------------------------------------------------
    // Offset / limit windowing
    // ---------------------------------------------------------------------------

    #[test]
    fn read_with_offset_skips_lines() {
        let f = write_temp("line1\nline2\nline3\nline4\n");
        let out = call(f.path().to_str().unwrap(), Some(2), None).unwrap();
        assert_eq!(out, "line3\nline4");
    }

    #[test]
    fn read_with_limit_caps_output() {
        let f = write_temp("a\nb\nc\nd\ne\n");
        let out = call(f.path().to_str().unwrap(), None, Some(3)).unwrap();
        // truncated — must start with the header
        assert!(out.starts_with("Lines 1-3 of 5"), "got: {out}");
        assert!(out.contains("a\nb\nc"));
    }

    #[test]
    fn read_offset_and_limit_combined() {
        let f = write_temp("a\nb\nc\nd\ne\n");
        let out = call(f.path().to_str().unwrap(), Some(1), Some(2)).unwrap();
        // offset=1, limit=2 → lines b, c; 2 more remain → truncation header
        assert!(out.contains("b\nc"), "got: {out}");
    }

    #[test]
    fn read_offset_beyond_file_end_returns_empty() {
        let f = write_temp("only one line\n");
        let out = call(f.path().to_str().unwrap(), Some(99), None).unwrap();
        assert_eq!(out, "");
    }

    #[test]
    fn read_limit_equal_to_line_count_produces_no_header() {
        let f = write_temp("x\ny\nz\n");
        let out = call(f.path().to_str().unwrap(), None, Some(3)).unwrap();
        // exactly fits — no truncation header
        assert!(!out.contains("Lines"), "got: {out}");
        assert_eq!(out, "x\ny\nz");
    }

    // ---------------------------------------------------------------------------
    // Truncation header format
    // ---------------------------------------------------------------------------

    #[test]
    fn read_truncation_header_shows_range_and_total() {
        // 10 lines, read only first 4
        let content = (1..=10)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let f = write_temp(&content);
        let out = call(f.path().to_str().unwrap(), None, Some(4)).unwrap();
        assert!(out.starts_with("Lines 1-4 of 10"), "got: {out}");
        assert!(out.contains("use offset=4 to read more"), "got: {out}");
    }

    #[test]
    fn read_truncation_header_reflects_offset() {
        let content = (1..=10)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let f = write_temp(&content);
        let out = call(f.path().to_str().unwrap(), Some(3), Some(3)).unwrap();
        // offset=3, limit=3 → lines 4-6 (1-based), 4 remain → header
        assert!(out.starts_with("Lines 4-6 of 10"), "got: {out}");
        assert!(out.contains("use offset=6 to read more"), "got: {out}");
    }

    // ---------------------------------------------------------------------------
    // Error cases
    // ---------------------------------------------------------------------------

    #[test]
    fn read_returns_error_for_missing_file() {
        let err = call("/nonexistent/path/file.txt", None, None).unwrap_err();
        assert!(err.to_string().contains("No such file") || err.to_string().contains("os error"));
    }
}
