//! 消息模型：文本 / 多模态内容、工具调用、推理链

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// 消息角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// 系统提示
    System,
    /// 用户输入
    User,
    /// 模型回复
    Assistant,
    /// 工具执行结果（配合工具调用使用）
    Tool,
    /// 开发者提示（部分新接口使用）
    Developer,
}

/// 消息内容
///
/// 支持三种形态，覆盖纯文本、多模态（图文）与「无内容」（例如只带 tool_calls 的
/// assistant 消息）：
/// - `Text`: `"你好"`
/// - `Parts`: `[{"type":"text","text":"..."},{"type":"image_url",...}]`
/// - `None`: `null`
#[derive(Debug, Clone, PartialEq)]
pub enum MessageContent {
    /// 无内容（序列化为 `null`）
    None,
    /// 纯文本
    Text(String),
    /// 多模态分段
    Parts(Vec<ContentPart>),
}

impl Default for MessageContent {
    fn default() -> Self {
        Self::None
    }
}

impl MessageContent {
    /// 是否为空内容（用于序列化时省略字段）
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// 提取纯文本视图（多模态内容会把所有文本分段拼接）
    pub fn as_text(&self) -> Option<String> {
        match self {
            Self::None => None,
            Self::Text(text) => Some(text.clone()),
            Self::Parts(parts) => {
                let mut out = String::new();
                for part in parts {
                    if let ContentPart::Text { text } = part {
                        out.push_str(text);
                    }
                }
                Some(out)
            }
        }
    }
}

impl From<String> for MessageContent {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for MessageContent {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<Vec<ContentPart>> for MessageContent {
    fn from(value: Vec<ContentPart>) -> Self {
        Self::Parts(value)
    }
}

impl Serialize for MessageContent {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::None => serializer.serialize_none(),
            Self::Text(text) => serializer.serialize_str(text),
            Self::Parts(parts) => parts.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for MessageContent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(match value {
            Value::Null => Self::None,
            Value::String(text) => Self::Text(text),
            Value::Array(_) => Self::Parts(
                serde_json::from_value(value).map_err(|e| D::Error::custom(e.to_string()))?,
            ),
            // 兜底：数字 / 布尔等非标准形态按字面量转文本，不让整条消息失败
            other => Self::Text(other.to_string()),
        })
    }
}

/// 多模态内容分段
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// 文本分段
    Text {
        /// 文本内容
        text: String,
    },
    /// 图片分段
    ImageUrl {
        /// 图片地址信息
        image_url: ImageUrl,
    },
}

/// 图片地址
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageUrl {
    /// URL 或 `data:` base64
    pub url: String,
    /// 细节等级：`auto` / `low` / `high`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ImageUrl {
    /// 用 URL 创建图片
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            detail: None,
        }
    }
}

/// 工具调用（模型返回的调用请求 / 回传给模型的历史调用）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// 调用 ID（回传工具结果时需要用 `tool_call_id` 对齐）
    #[serde(default)]
    pub id: String,
    /// 类型，目前固定为 `function`
    #[serde(rename = "type", default = "default_function_type")]
    pub kind: String,
    /// 函数调用信息
    #[serde(default)]
    pub function: FunctionCall,
}

impl ToolCall {
    /// 构造一个函数调用
    pub fn function(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: default_function_type(),
            function: FunctionCall {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }
}

/// 函数调用信息
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    /// 函数名
    #[serde(default)]
    pub name: String,
    /// 参数（JSON 字符串，流式下由多个增量拼接而成）
    #[serde(default)]
    pub arguments: String,
}

/// 聊天消息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// 角色
    pub role: Role,
    /// 正文（`None` 表示不带正文）
    #[serde(default, skip_serializing_if = "MessageContent::is_none")]
    pub content: MessageContent,
    /// 思考内容（部分推理模型要求多轮对话时回传上一轮推理链）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// 参与者名称（多参与者场景 / 工具名）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 工具调用列表
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// 工具结果对应的调用 ID（`role = tool` 时必填）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    /// 创建系统提示消息
    pub fn system(content: impl Into<String>) -> Self {
        Self::from_text(Role::System, content)
    }

    /// 创建用户消息
    pub fn user(content: impl Into<String>) -> Self {
        Self::from_text(Role::User, content)
    }

    /// 创建助手消息
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::from_text(Role::Assistant, content)
    }

    /// 创建带思考内容的助手消息（用于多轮对话回传推理链）
    pub fn assistant_with_reasoning(
        content: impl Into<String>,
        reasoning: impl Into<String>,
    ) -> Self {
        Self {
            reasoning_content: Some(reasoning.into()),
            ..Self::assistant(content)
        }
    }

    /// 创建只带工具调用的助手消息（正文为 `null`）
    pub fn assistant_with_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::None,
            reasoning_content: None,
            name: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    /// 创建工具结果消息
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into().into(),
            reasoning_content: None,
            name: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    /// 创建多模态消息（图文）
    pub fn user_with_parts(parts: Vec<ContentPart>) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::Parts(parts),
            reasoning_content: None,
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// 设置参与者名称
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// 设置正文
    pub fn with_content(mut self, content: impl Into<MessageContent>) -> Self {
        self.content = content.into();
        self
    }

    /// 纯文本正文视图
    pub fn text(&self) -> &str {
        match &self.content {
            MessageContent::Text(text) => text,
            _ => "",
        }
    }

    /// 通用构造：指定角色与文本
    fn from_text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into().into(),
            reasoning_content: None,
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }
}

/// 工具类型默认值：`function`
pub(crate) fn default_function_type() -> String {
    "function".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 纯文本消息序列化成字符串，而不是数组
    #[test]
    fn test_text_content_wire_format() {
        let msg = ChatMessage::user("你好");
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "你好");
        assert!(json.get("reasoning_content").is_none());
    }

    /// 多模态消息序列化成 parts 数组
    #[test]
    fn test_parts_content_wire_format() {
        let msg = ChatMessage::user_with_parts(vec![
            ContentPart::Text {
                text: "描述这张图".into(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrl::new("https://example.com/a.png"),
            },
        ]);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(
            json["content"][1]["image_url"]["url"],
            "https://example.com/a.png"
        );
    }

    /// 助手工具调用消息：content 序列化为 null，tool_calls 必须是数组
    #[test]
    fn test_assistant_tool_calls_wire_format() {
        let msg = ChatMessage::assistant_with_tool_calls(vec![ToolCall::function(
            "call_1",
            "get_weather",
            r#"{"city":"北京"}"#,
        )]);
        let json = serde_json::to_value(&msg).unwrap();
        assert!(json["content"].is_null());
        assert_eq!(json["tool_calls"][0]["type"], "function");
        assert_eq!(json["tool_calls"][0]["function"]["name"], "get_weather");
    }

    /// 工具结果消息必须带 tool_call_id
    #[test]
    fn test_tool_message_wire_format() {
        let msg = ChatMessage::tool("call_1", r#"{"temp": 20}"#);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "tool");
        assert_eq!(json["tool_call_id"], "call_1");
    }

    /// 前端可能传 camelCase 形态的推理链，反序列化后仍需可用
    #[test]
    fn test_content_accepts_array_and_null() {
        let msg: ChatMessage = serde_json::from_value(serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "hi"}],
        }))
        .unwrap();
        assert_eq!(msg.content.as_text().as_deref(), Some("hi"));

        let msg: ChatMessage =
            serde_json::from_value(serde_json::json!({"role": "assistant", "content": null}))
                .unwrap();
        assert!(msg.content.is_none());
    }
}
