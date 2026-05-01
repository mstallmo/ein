// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

mod native;

pub use ein_core::types::{ToolDef, ToolResult};
pub use native::NativeToolSet;

use async_trait::async_trait;

use crate::agents::AgentEventHandler;

/// A single in-process tool. Implement this for simple, `Send + Sync` tools
/// and register them with [`DefaultToolSet`].
///
/// For advanced use cases (e.g. WASM-backed tools that require exclusive,
/// non-`Send` store access), implement [`ToolSet`] directly instead.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolDef;
    async fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult>;

    /// The name of the parameter to extract and display next to the tool name
    /// in client UIs. Return `None` (the default) to show only the tool name.
    fn primary_arg(&self) -> Option<&str> {
        None
    }
}

/// The execution boundary for tool calls.
///
/// The agent calls [`ToolSet::call_tool`] by name rather than borrowing
/// individual tool objects. This gives implementors full ownership semantics
/// during execution — enabling registries backed by WASM stores, thread
/// pools, or any other `!Send`/exclusive-access mechanism.
///
/// For the common case of simple in-process tools, use [`DefaultToolSet`],
/// which implements this trait over a collection of [`Tool`] objects.
#[async_trait]
pub trait ToolSet {
    fn schemas(&self) -> Vec<ToolDef>;

    async fn call_tool(&mut self, name: &str, id: &str, args: &str) -> anyhow::Result<ToolResult>;

    fn set_event_handler(&mut self, _handler: AgentEventHandler) {}

    async fn cleanup(mut self)
    where
        Self: Sized,
    {
    }

    /// Resolves the display argument value for a tool call by looking up the
    /// tool's declared `primary_arg` param and extracting it from `args` JSON.
    /// Returns `None` for unknown tools or tools without a primary arg.
    fn display_arg_for(&self, _tool_name: &str, _args: &str) -> Option<String> {
        None
    }
}
