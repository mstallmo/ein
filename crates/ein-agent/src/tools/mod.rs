// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use async_trait::async_trait;
use serde::{
    Deserialize, Serialize,
    de::{self, MapAccess, Visitor},
    ser::{self, SerializeStruct},
};
use serde_json;

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

#[derive(Debug, Clone)]
pub enum ToolDef {
    Function {
        name: String,
        description: String,
        parameters: ToolFunctionParams,
    },
}

impl ToolDef {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> ToolFunctionBuilder {
        ToolFunctionBuilder::new(name.into(), description.into())
    }
}

// TODO: Add feature flag to select the seralization format (OpenAI(default) vs Anthropic)
impl<'de> Deserialize<'de> for ToolDef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct FunctionBody {
            name: String,
            description: String,
            parameters: ToolFunctionParams,
        }

        struct ToolDefVisitor;

        impl<'de> Visitor<'de> for ToolDefVisitor {
            type Value = ToolDef;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(r#"a map with "type" and "function" fields"#)
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut type_field: Option<String> = None;
                let mut function_body: Option<FunctionBody> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "type" => {
                            type_field = Some(map.next_value()?);
                        }
                        "function" => {
                            function_body = Some(map.next_value()?);
                        }
                        _ => {
                            let _ = map.next_value::<de::IgnoredAny>()?;
                        }
                    }
                }

                let type_val = type_field.ok_or_else(|| de::Error::missing_field("type"))?;

                match type_val.as_str() {
                    "function" => {
                        let body =
                            function_body.ok_or_else(|| de::Error::missing_field("function"))?;
                        Ok(ToolDef::Function {
                            name: body.name,
                            description: body.description,
                            parameters: body.parameters,
                        })
                    }
                    other => Err(de::Error::unknown_variant(other, &["function"])),
                }
            }
        }

        deserializer.deserialize_map(ToolDefVisitor)
    }
}

impl ser::Serialize for ToolDef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: ser::Serializer,
    {
        match self {
            ToolDef::Function {
                name,
                description,
                parameters,
            } => {
                // A temporary local struct to serialize the nested "function" object.
                struct FunctionBody<'a> {
                    name: &'a str,
                    description: &'a str,
                    parameters: &'a ToolFunctionParams,
                }

                impl<'a> ser::Serialize for FunctionBody<'a> {
                    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
                    where
                        S: ser::Serializer,
                    {
                        let mut s = serializer.serialize_struct("FunctionBody", 3)?;
                        s.serialize_field("name", self.name)?;
                        s.serialize_field("description", self.description)?;
                        s.serialize_field("parameters", self.parameters)?;
                        s.end()
                    }
                }

                // Top-level object: { "type": "function", "function": { ... } }
                let mut s = serializer.serialize_struct("ToolDef", 2)?;
                s.serialize_field("type", "function")?;
                s.serialize_field(
                    "function",
                    &FunctionBody {
                        name,
                        description,
                        parameters,
                    },
                )?;
                s.end()
            }
        }
    }
}

struct ParamDef {
    name: String,
    param_type: String,
    description: String,
    required: bool,
}

pub struct ToolFunctionBuilder {
    name: String,
    description: String,
    props: Vec<ParamDef>,
}

impl ToolFunctionBuilder {
    pub fn new(name: String, description: String) -> Self {
        Self {
            name,
            description,
            props: Vec::new(),
        }
    }

    pub fn param(
        mut self,
        name: impl Into<String>,
        param_type: impl Into<String>,
        description: impl Into<String>,
        required: bool,
    ) -> Self {
        self.props.push(ParamDef {
            name: name.into(),
            param_type: param_type.into(),
            description: description.into(),
            required,
        });
        self
    }

    pub fn build(self) -> ToolDef {
        let mut props = ToolFuncProps::new();
        let mut required_props = Vec::new();

        for param_def in self.props {
            props.add_prop(
                param_def.name.clone(),
                ToolFuncPropInfo::new(param_def.param_type, param_def.description),
            );

            if param_def.required {
                required_props.push(param_def.name)
            }
        }

        let params = ToolFunctionParams::Object {
            properties: props,
            required: required_props,
        };

        ToolDef::Function {
            name: self.name,
            description: self.description,
            parameters: params,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
pub enum ToolFunctionParams {
    Object {
        properties: ToolFuncProps,
        required: Vec<String>,
    },
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct ToolFuncProps(collections::HashMap<String, ToolFuncPropInfo>);

impl ToolFuncProps {
    pub fn new() -> Self {
        Self(collections::HashMap::new())
    }

    pub fn add_prop(&mut self, name: impl ToString, info: ToolFuncPropInfo) {
        self.0.insert(name.to_string(), info);
    }

    pub fn props(&self) -> &collections::HashMap<String, ToolFuncPropInfo> {
        &self.0
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolFuncPropInfo {
    #[serde(rename = "type")]
    pub prop_type: String,
    pub description: String,
}

impl ToolFuncPropInfo {
    pub fn new(prop_type: String, description: String) -> Self {
        Self {
            prop_type,
            description,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolResult {
    tool_call_id: String,
    pub content: String,
    /// Optional tool-specific data forwarded to the client as-is.
    /// Not sent to the LLM — the server extracts and routes it separately.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

impl ToolResult {
    pub fn new(id: impl ToString, content: String) -> Self {
        Self {
            tool_call_id: id.to_string(),
            content,
            metadata: None,
        }
    }

    pub fn with_metadata(id: impl ToString, content: String, metadata: serde_json::Value) -> Self {
        Self {
            tool_call_id: id.to_string(),
            content,
            metadata: Some(metadata),
        }
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
