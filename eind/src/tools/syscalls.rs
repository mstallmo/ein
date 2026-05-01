// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use ein_agent::AgentEvent;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::ToolState;

impl super::bindings::ein::host::host::Host for ToolState {
    async fn log(&mut self, msg: String) {
        println!("[plugin] {msg}");
    }
}

impl super::bindings::ein::plugin::process::Host for ToolState {
    async fn spawn(&mut self, args: String) -> Result<String, String> {
        println!("[plugin] spawning new process: {args}");

        let mut child = tokio::process::Command::new("sh")
            .args(["-c", &args])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;

        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let mut stdout_lines = BufReader::new(stdout).lines();
        let mut stderr_lines = BufReader::new(stderr).lines();
        let mut stdout_buf = String::new();
        let mut stderr_buf = String::new();
        let (mut stdout_done, mut stderr_done) = (false, false);

        while !stdout_done || !stderr_done {
            tokio::select! {
                line = stdout_lines.next_line(), if !stdout_done => match line {
                    Ok(Some(l)) => {
                        stdout_buf.push_str(&l);
                        stdout_buf.push('\n');

                        if let Some(handler) = &self.event_handler && let Some(tool_call_id) = &self.current_call_id {
                            let event = AgentEvent::ToolOutputChunk {
                                tool_call_id: tool_call_id.clone(),
                                output: l,
                            };

                            handler(event).await;
                        }
                    }
                    _ => stdout_done = true,
                },
                line = stderr_lines.next_line(), if !stderr_done => match line {
                    Ok(Some(l)) => {
                        stderr_buf.push_str(&l);
                        stderr_buf.push('\n');
                    }
                    _ => stderr_done = true,
                },
            }
        }

        let status = child.wait().await.map_err(|e| e.to_string())?;

        Ok(format!(
            "Exit code: {}\nStdout:\n{stdout_buf}\nStderr:\n{stderr_buf}",
            status.code().unwrap_or(-1)
        ))
    }
}
