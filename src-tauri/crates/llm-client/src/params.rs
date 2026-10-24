//! 请求参数模型：采样参数、工具定义、结构化输出

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::message::default_function_type;

/// 流式选项
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamOptions {
    /// 是否在流末尾返回 usage 统计
    ///
    /// 部分老版本网关不认识 `stream_options` 字段，会直接返回 400，
    /// 这类服务需要把它关掉（此时拿不到 usage，token 指标会缺省）。
    pub include_usage: bool,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            include_usage: true,
        }
    }
}

/// 结构化输出格式
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    /// 普通文本
    Text,
    /// 强制返回合法 JSON 对象
    JsonObject,
    /// 按指定 JSON Schema 输出
    JsonSchema {
        /// JSON Schema 定义（结构随厂商略有差异，原样透传）
        json_schema: Value,
    },
}

/// 工具选择策略
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    /// `auto` / `none` / `required`
    Mode(ToolChoiceMode),
    /// 强制调用指定函数
    Function(ToolChoiceFunction),
}

impl ToolChoice {
    /// 由模型自行决定
    pub fn auto() -> Self {
        Self::Mode(ToolChoiceMode::Auto)
    }

    /// 不调用工具
    pub fn none() -> Self {
        Self::Mode(ToolChoiceMode::None)
    }

    /// 必须调用工具
    pub fn required() -> Self {
        Self::Mode(ToolChoiceMode::Required)
    }

    /// 强制调用指定函数
    pub fn function(name: impl Into<String>) -> Self {
        Self::Function(ToolChoiceFunction {
            kind: default_function_type(),
            function: NamedFunction { name: name.into() },
        })
    }
}

/// 工具选择模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoiceMode {
    /// 不调用
    None,
    /// 由模型决定
    Auto,
    /// 必须调用
    Required,
}

/// 强制调用指定函数
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolChoiceFunction {
    /// 固定为 `function`
    #[serde(rename = "type")]
    pub kind: String,
    /// 函数名
    pub function: NamedFunction,
}

/// 只带名字的函数引用
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedFunction {
    /// 函数名
    pub name: String,
}

/// 工具定义
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    /// 工具类型，目前固定为 `function`
    #[serde(rename = "type", default = "default_function_type")]
    pub kind: String,
    /// 函数定义
    pub function: FunctionDefinition,
}

impl Tool {
    /// 定义一函数工具
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        Self {
            kind: default_function_type(),
            function: FunctionDefinition {
                name: name.into(),
                description: Some(description.into()),
                parameters: Some(parameters),
            },
        }
    }
}

/// 函数定义
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionDefinition {
    /// 函数名
    pub name: String,
    /// 功能描述（模型据此判断何时调用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 参数的 JSON Schema
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

/// 大模型调用参数
///
/// `None` 表示该字段不传给 API（使用服务端默认值）。
/// 厂商特有参数通过 [`ChatParams::extra`] 透传，例如 `top_k`、`repetition_penalty`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatParams {
    /// 模型名称，如 `gpt-4o`、`deepseek-reasoner`、`qwen-plus`
    pub model: String,

    /// 采样温度 [0.0, 2.0]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// 核采样概率 [0.0, 1.0]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    /// 最大生成 token 数（旧字段）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// 最大生成 token 数（新字段，推理模型可能需要）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,

    /// 频率惩罚 [-2.0, 2.0]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,

    /// 存在惩罚 [-2.0, 2.0]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,

    /// 随机种子（需服务端支持）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,

    /// 停止词
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,

    /// 生成候选数量（流式多候选会通过 `index` 区分）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,

    /// 工具列表
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,

    /// 工具选择策略
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,

    /// 是否允许并行调用多个工具
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,

    /// 结构化输出格式
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,

    /// 是否返回对数概率
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<bool>,

    /// 返回概率最高的 N 个候选 token
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u32>,

    /// 调用方标识（用于服务端做滥用检测）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,

    /// 流式选项：由库在流式请求时合并进请求体，不会直接序列化
    #[serde(skip)]
    pub stream_options: StreamOptions,

    /// 扩展参数：任意自定义字段会合并到请求体顶层（后写入的同名键会覆盖前面的）
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ChatParams {
    /// 指定模型创建参数
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Default::default()
        }
    }

    /// 设置采样温度
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// 设置最大生成 token 数
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// 设置工具列表
    pub fn with_tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// 设置工具选择策略
    pub fn with_tool_choice(mut self, choice: ToolChoice) -> Self {
        self.tool_choice = Some(choice);
        self
    }

    /// 设置结构化输出格式
    pub fn with_response_format(mut self, format: ResponseFormat) -> Self {
        self.response_format = Some(format);
        self
    }

    /// 关闭流式 usage 统计（兼容不支持 `stream_options` 的网关）
    pub fn without_stream_usage(mut self) -> Self {
        self.stream_options.include_usage = false;
        self
    }

    /// 追加一个厂商特有参数
    pub fn with_extra(mut self, key: impl Into<String>, value: Value) -> Self {
        self.extra.insert(key.into(), value);
        self
    }
}

/// 向量化输入
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingInput {
    /// 单条文本
    Text(String),
    /// 多条文本（批量）
    Texts(Vec<String>),
}

/// 向量化请求参数
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingParams {
    /// 向量模型名
    pub model: String,
    /// 输入文本
    pub input: EmbeddingInput,
    /// 输出维度（部分模型支持降维）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,
    /// 扩展参数
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl EmbeddingParams {
    /// 用单条文本创建请求
    pub fn text(model: impl Into<String>, input: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            input: EmbeddingInput::Text(input.into()),
            dimensions: None,
            extra: Map::new(),
        }
    }

    /// 用多条文本创建请求
    pub fn texts<I, S>(model: impl Into<String>, input: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            model: model.into(),
            input: EmbeddingInput::Texts(input.into_iter().map(Into::into).collect()),
            dimensions: None,
            extra: Map::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 工具列表与 tool_choice 必须按 OpenAI 协议序列化
    #[test]
    fn test_tool_params_wire_format() {
        let params = ChatParams::new("gpt-4o")
            .with_tools(vec![Tool::function(
                "get_weather",
                "查询天气",
                serde_json::json!({"type": "object", "properties": {}}),
            )])
            .with_tool_choice(ToolChoice::auto());
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(json["tool_choice"], "auto");
    }

    /// 强制调用指定函数时，tool_choice 是一个对象
    #[test]
    fn test_tool_choice_function_wire_format() {
        let params = ChatParams::new("m").with_tool_choice(ToolChoice::function("read_file"));
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["tool_choice"]["type"], "function");
        assert_eq!(json["tool_choice"]["function"]["name"], "read_file");
    }

    /// 扩展字段必须平铺到请求体顶层
    #[test]
    fn test_extra_is_flattened() {
        let params = ChatParams::new("m").with_extra("top_k", serde_json::json!(20));
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["top_k"], 20);
        assert!(
            json.get("stream_options").is_none(),
            "流式选项不应在参数中直接序列化"
        );
    }

    /// 参数可由 JSON 反序列化（便于从配置文件加载）
    #[test]
    fn test_params_deserialize_from_config() {
        let params: ChatParams = serde_json::from_value(serde_json::json!({
            "model": "qwen-plus",
            "temperature": 0.2,
            "repetition_penalty": 1.05
        }))
        .unwrap();
        assert_eq!(params.model, "qwen-plus");
        assert_eq!(params.temperature, Some(0.2));
        assert!(params.extra.contains_key("repetition_penalty"));
    }
}
