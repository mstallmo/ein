use anyhow::Context;
use std::process;

impl crate::bindings::ein::plugin::host::Host for crate::HarnessState {
    async fn log(&mut self, msg: String) {
        println!("[plugin] {msg}");
    }

    async fn spawn(&mut self, args: String) -> Result<String, String> {
        println!("[plugin] spawning new process: {args}");

        let message = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let output = process::Command::new("sh")
                .args(["-c", &args])
                .output()
                .with_context(|| "Failed to create `sh` command")?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            let message = format!(
                "Exit code: {}\nStdout:\n{stdout}\nStderr:\n{stderr}",
                output.status.code().unwrap_or(-1)
            );

            Ok(message)
        })
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())?;

        Ok(message)
    }
}
