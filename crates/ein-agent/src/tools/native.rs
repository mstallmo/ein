// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use std::collections;

use async_trait::async_trait;
use ein_core::types::{ToolDef, ToolResult};

use crate::tools::{Tool, ToolSet};

// TODO:
// - Refactor WASM tool runtime implementation from the gRPC server into this crate
// - Put WASM tool runtime behind a feature flag
/// Native [`ToolSet`] implementation backed by a collection of [`Tool`]
/// trait objects.
#[derive(Default)]
pub struct NativeToolSet(collections::HashMap<String, Box<dyn Tool>>);

impl NativeToolSet {
    pub fn insert(&mut self, tool: impl Tool + 'static) {
        self.0.insert(tool.name().to_string(), Box::new(tool));
    }
}

#[async_trait]
impl ToolSet for NativeToolSet {
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
