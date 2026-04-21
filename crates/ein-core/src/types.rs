use serde::{Deserialize, Serialize};

use std::collections;

// -------------------------------
// Model Completion Requests
// -------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    /// Full conversation history. Typed as `Vec<Message>` so the contract is
    /// explicit and compiler-enforced; serialises to OpenAI chat-completion
    /// format, which OpenAI-compatible plugins can send verbatim.
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub max_tokens: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    #[serde(default)]
    pub choices: Vec<Choice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Present when the upstream API returns an error object (e.g. 402
    /// insufficient credits). The server emits an `AgentError` event rather
    /// than crashing when this field is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: Option<usize>,
    pub finish_reason: FinishReason,
    pub message: Message,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    #[serde(other)]
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
pub enum ToolCall {
    Function {
        id: String,
        /// Position index from streaming response deltas. Not part of the
        /// OpenAI request schema; omitted when serialising back to the API.
        #[serde(skip_serializing, default)]
        index: usize,
        function: FunctionCall,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Raw JSON string containing the arguments chosen by the model.
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// -------------------------------
// Tools
// -------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
pub enum ToolDef {
    Function { function: ToolFunction },
}

impl ToolDef {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> ToolFunctionBuilder {
        ToolFunctionBuilder::new(name.into(), description.into())
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

        let tool_func = ToolFunction {
            name: self.name,
            description: self.description,
            parameters: params,
        };

        ToolDef::Function {
            function: tool_func,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: ToolFunctionParams,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
pub enum ToolFunctionParams {
    Object {
        properties: ToolFuncProps,
        required: Vec<String>,
    },
}

#[derive(Debug, Default, Clone, Eq, PartialEq, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
pub struct ToolFuncPropInfo {
    #[serde(rename = "type")]
    prop_type: String,
    description: String,
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
