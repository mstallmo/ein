// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

pub use ein_core::types::{ToolDef, ToolResult};

use async_trait::async_trait;

use std::collections;

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

    async fn unload(mut self)
    where
        Self: Sized,
    {
        // No-op for `ToolSet` impls that don't need to release resources
    }
}

/// Default [`ToolSet`] implementation backed by a collection of [`Tool`]
/// trait objects. Suitable for simple in-process tools.
#[derive(Default)]
pub struct DefaultToolSet(collections::HashMap<String, Box<dyn Tool>>);

impl DefaultToolSet {
    pub fn insert(&mut self, tool: impl Tool + 'static) {
        self.0.insert(tool.name().to_string(), Box::new(tool));
    }
}

#[async_trait]
impl ToolSet for DefaultToolSet {
    fn schemas(&self) -> Vec<ToolDef> {
        self.0.values().map(|v| v.schema()).collect()
    }

    async fn call_tool(&mut self, name: &str, id: &str, args: &str) -> anyhow::Result<ToolResult> {
        match self.0.get(name) {
            Some(tool) => tool.call(id, args).await,
            None => Err(anyhow::anyhow!("tool not found: {name}")),
        }
    }
}
