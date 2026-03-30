use ein_proto::ein::{AgentEvent, ToolOutputChunk, agent_event::Event};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

impl crate::bindings::ein::host::host::Host for crate::HarnessState {
    async fn log(&mut self, msg: String) {
        println!("[plugin] {msg}");
    }
}

impl crate::bindings::ein::plugin::process::Host for crate::HarnessState {
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
        let chunk_tx = self.chunk_tx.clone();
        let tool_call_id = self.tool_call_id.clone();

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
                        if let Some(tx) = &chunk_tx {
                            let _ = tx.send(Ok(AgentEvent {
                                event: Some(Event::ToolOutputChunk(ToolOutputChunk {
                                    tool_call_id: tool_call_id.clone(),
                                    output: l,
                                })),
                            })).await;
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

impl crate::model_client_bindings::ein::host::host::Host for crate::ModelClientHarnessState {
    async fn log(&mut self, msg: String) {
        println!("[model client] {msg}");
    }
}
