use thiserror::Error;

// TODO: Cleanup error handling in the entire crate
// after initial impl

/// Errors that can occur during the agent loop.
#[derive(Error, Debug)]
pub enum AgentError {
    /// The model client returned an error (e.g. network failure, API error).
    #[error("{0}")]
    ModelClient(String),
    /// A tool execution failed.
    #[error("{0}")]
    Tool(#[from] ToolError),
    /// The model returned a finish reason the agent does not know how to handle.
    #[error("{0}")]
    UnsupportedFinishReason(String),
}

/// Errors that can occur when executing a tool call.
#[derive(Error, Debug)]
pub enum ToolError {
    /// The tool ran but produced an error result.
    #[error("Error: '{0}'")]
    Execution(String),
    /// The model requested a tool that is not registered.
    #[error("Error: tool '{0}' not found")]
    Unknown(String),
}
