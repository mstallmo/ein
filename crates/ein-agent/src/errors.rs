use thiserror::Error;

// TODO: Cleanup error handling in the entire crate
// after initial impl
#[derive(Error, Debug)]
pub enum AgentError {
    #[error("{0}")]
    ModelClient(String),
    #[error("{0}")]
    Tool(#[from] ToolError),
    #[error("{0}")]
    UnsupportedFinishReason(String),
}

#[derive(Error, Debug)]
pub enum ToolError {
    #[error("Error: '{0}'")]
    Execution(String),
    #[error("Error: tool '{0}' not found")]
    Unknown(String),
}
