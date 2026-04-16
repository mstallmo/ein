// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

pub mod syscalls {
    pub use crate::tool::__wit::ein::host::host::log;
    pub use crate::tool::__wit::ein::plugin::process::spawn;
}

#[doc(hidden)]
pub mod __wit {
    use super::ConstructableToolPlugin;
    use wit_bindgen::generate;

    generate!({
        world: "plugin",
        path: "../../wit/plugin",
        export_macro_name: "__export_plugin_inner",
        pub_export_macro: true,
        default_bindings_module: "ein_plugin::tool::__wit",
        with: { "ein:host/host": generate }
    });

    impl<T> exports::tool::Guest for T
    where
        T: exports::tool::GuestTool,
    {
        type Tool = Self;
    }

    impl<T> exports::tool::GuestTool for T
    where
        T: ConstructableToolPlugin + 'static,
    {
        fn new() -> Self {
            T::new()
        }

        fn name(&self) -> String {
            self.name().to_string()
        }

        fn schema(&self) -> String {
            match serde_json::to_string(&self.schema()) {
                Ok(val) => val,
                Err(err) => {
                    eprintln!("Failed to serialize schema: {err}");
                    "".to_string()
                }
            }
        }

        fn enable_chunk_sender(&self) -> bool {
            self.enable_chunk_sender()
        }

        fn call(&self, id: String, args: String) -> Result<String, String> {
            let res = self.call(&id, &args).map_err(|err| err.to_string())?;
            serde_json::to_string(&res).map_err(|err| err.to_string())
        }
    }
}

#[macro_export]
macro_rules! export_tool {
    ($($t:tt)*) => {
        $crate::tool::__wit::__export_plugin_inner!($($t)*);
    };
}

use serde::{
    Deserialize, Serialize,
    de::{self, MapAccess, Visitor},
    ser::{self, SerializeStruct},
};
use std::collections;

pub trait ConstructableToolPlugin: ToolPlugin {
    fn new() -> Self;
}

// WASM plugin for Ein implementation detail
pub trait ToolPlugin: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolDef;
    fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult>;

    fn enable_chunk_sender(&self) -> bool {
        false
    }
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
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolResult {
    role: Role,
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
            role: Role::Tool,
            tool_call_id: id.to_string(),
            content,
            metadata: None,
        }
    }

    pub fn with_metadata(id: impl ToString, content: String, metadata: serde_json::Value) -> Self {
        Self {
            role: Role::Tool,
            tool_call_id: id.to_string(),
            content,
            metadata: Some(metadata),
        }
    }
}
