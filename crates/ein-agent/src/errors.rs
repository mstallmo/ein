use thiserror::Error;

// TODO: Cleanup error handling in the entire crate
// after initial impl
#[derive(Error, Debug)]
pub enum AgentError {
    #[error("{0}")]
    ModelClient(String),
    #[error("{0}")]
    UnsupportedFinishReason(String),
}
