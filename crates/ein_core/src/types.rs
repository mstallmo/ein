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

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------
    // ToolFunctionBuilder
    // ---------------------------------------------------------------------------

    #[test]
    fn builder_produces_function_tool_def() {
        let tool = ToolDef::function("my_tool", "does a thing").build();
        assert!(matches!(tool, ToolDef::Function { .. }));
    }

    #[test]
    fn builder_required_param_appears_in_required_array() {
        let tool = ToolDef::function("t", "d")
            .param("req_param", "string", "a required param", true)
            .build();

        let ToolDef::Function { function } = tool;
        let ToolFunctionParams::Object { required, .. } = &function.parameters;
        assert!(required.contains(&"req_param".to_string()));
    }

    #[test]
    fn builder_optional_param_absent_from_required_array() {
        let tool = ToolDef::function("t", "d")
            .param("opt_param", "string", "an optional param", false)
            .build();

        let ToolDef::Function { function } = tool;
        let ToolFunctionParams::Object { required, .. } = &function.parameters;
        assert!(!required.contains(&"opt_param".to_string()));
    }

    #[test]
    fn builder_param_appears_in_properties() {
        let tool = ToolDef::function("t", "d")
            .param("my_arg", "integer", "an arg", true)
            .build();

        let ToolDef::Function { function } = tool;
        let ToolFunctionParams::Object { properties, .. } = &function.parameters;
        assert!(properties.props().contains_key("my_arg"));
    }

    #[test]
    fn builder_serializes_to_openai_schema_shape() {
        let tool = ToolDef::function("bash", "run a shell command")
            .param("command", "string", "the command to run", true)
            .build();

        let v = serde_json::to_value(&tool).unwrap();
        assert_eq!(v["type"], "function");
        assert_eq!(v["function"]["name"], "bash");
        assert_eq!(v["function"]["parameters"]["type"], "object");
        assert_eq!(
            v["function"]["parameters"]["properties"]["command"]["type"],
            "string"
        );
        assert_eq!(v["function"]["parameters"]["required"][0], "command");
    }

    // ---------------------------------------------------------------------------
    // Message serialization
    // ---------------------------------------------------------------------------

    #[test]
    fn message_round_trips_for_each_role() {
        for role in [Role::System, Role::User, Role::Assistant, Role::Tool] {
            let msg = Message {
                role: role.clone(),
                content: Some("hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
            };
            let json = serde_json::to_string(&msg).unwrap();
            let decoded: Message = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded.role, role);
            assert_eq!(decoded.content.as_deref(), Some("hello"));
        }
    }

    #[test]
    fn message_omits_none_fields_from_json() {
        let msg = Message {
            role: Role::User,
            content: Some("hi".to_string()),
            tool_calls: None,
            tool_call_id: None,
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert!(!v.as_object().unwrap().contains_key("tool_calls"));
        assert!(!v.as_object().unwrap().contains_key("tool_call_id"));
    }

    // ---------------------------------------------------------------------------
    // FinishReason
    // ---------------------------------------------------------------------------

    #[test]
    fn finish_reason_unknown_value_deserializes_as_unsupported() {
        let json = r#""length""#;
        let reason: FinishReason = serde_json::from_str(json).unwrap();
        assert!(matches!(reason, FinishReason::Unsupported));
    }

    #[test]
    fn finish_reason_known_values_round_trip() {
        for (s, expected) in [
            ("\"stop\"", FinishReason::Stop),
            ("\"tool_calls\"", FinishReason::ToolCalls),
        ] {
            let reason: FinishReason = serde_json::from_str(s).unwrap();
            assert!(matches!(
                (reason, expected),
                (FinishReason::Stop, FinishReason::Stop)
                    | (FinishReason::ToolCalls, FinishReason::ToolCalls)
            ));
        }
    }

    // ---------------------------------------------------------------------------
    // CompletionRequest / CompletionResponse
    // ---------------------------------------------------------------------------

    #[test]
    fn completion_request_round_trips() {
        let req = CompletionRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 1024,
            messages: vec![Message {
                role: Role::User,
                content: Some("hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
            }],
            tools: vec![],
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: CompletionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.model, "gpt-4o");
        assert_eq!(decoded.max_tokens, 1024);
        assert_eq!(decoded.messages.len(), 1);
    }

    #[test]
    fn completion_response_error_field_round_trips() {
        let resp = CompletionResponse {
            choices: vec![],
            usage: None,
            error: Some(serde_json::json!({"message": "insufficient credits"})),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: CompletionResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.error.unwrap()["message"], "insufficient credits");
    }
}
