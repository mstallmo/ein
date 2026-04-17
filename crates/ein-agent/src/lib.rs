// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! Core agent loop.
//!
//! [`run_agent`] drives a single conversation turn: it sends the current
//! message history to the LLM, streams back any text content as
//! [`ContentDelta`] events, executes every tool call the model requests, and
//! then repeats until the model signals [`FinishReason::Stop`].
//!
//! ## Message flow
//!
//! ```text
//! caller                   run_agent                LLM
//!   │                         │                      │
//!   │── messages ────────────►│── POST /chat/comp ──►│
//!   │                         │◄─ Choice ────────────│
//!   │                         │                      │
//!   │   FinishReason::ToolCalls:                     │
//!   │◄─ ContentDelta (opt) ───│                      │
//!   │◄─ ToolCallStart ────────│                      │
//!   │     (execute tool)      │                      │
//!   │◄─ ToolCallEnd ──────────│                      │
//!   │     (append result, loop again)                │
//!   │                         │                      │
//!   │   FinishReason::Stop:                          │
//!   │◄─ AgentFinished ────────│                      │
//!   │      (return)           │                      │
//! ```

pub mod agents;
pub mod errors;
pub mod model_clients;
pub mod tools;

pub use agents::{Agent, AgentBuilder, SessionParams};
pub use async_trait::async_trait;
