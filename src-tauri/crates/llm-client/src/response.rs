//! 响应模型：非流式补全、token 统计、模型列表、向量化

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::message::ToolCall;

/// 反序列化辅助：把显式 `null` 当成 0
///
/// `#[serde(default)]` 只能处理「字段缺失」，遇到显式 `null` 仍会整体报错，
/// 而部分厂商的 usage / timings 确实会返回 null。
fn null_to_zero<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<u64>::deserialize(deserializer)?.unwrap_or(0))
}

/// 同上，用于浮点字段
fn null_to_zero_f64<'de, D>(deserializer: D) -> std::result::Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<f64>::deserialize(deserializer)?.unwrap_or(0.0))
}

/// Prompt 阶段的 token 明细（部分 API 提供，用于区分 cache 命中）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PromptTokensDetails {
    /// cache 命中的 prompt token 数（命中部分通常不参与真实 prefill 计算）
    #[serde(default)]
    pub cached_tokens: Option<u64>,
    /// 音频输入 token 数
    #[serde(default)]
    pub audio_tokens: Option<u64>,
}

/// 补全阶段的 token 明细
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompletionTokensDetails {
    /// 思考/推理占用的 token 数（推理模型会返回）
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
    /// 音频输出 token 数
    #[serde(default)]
    pub audio_tokens: Option<u64>,
    /// 被接受的预测 token 数
    #[serde(default)]
    pub accepted_prediction_tokens: Option<u64>,
    /// 被拒绝的预测 token 数
    #[serde(default)]
    pub rejected_prediction_tokens: Option<u64>,
}

/// Token 使用量统计
///
/// 所有字段都容忍缺失与 null：不同厂商的字段并不统一，
/// 缺少单个字段不应让整条响应解析失败。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt 阶段消耗的 token 数（通常包含 cache 命中部分）
    #[serde(default, deserialize_with = "null_to_zero")]
    pub prompt_tokens: u64,
    /// 补全阶段消耗的 token 数（推理模型会包含思考 token）
    #[serde(default, deserialize_with = "null_to_zero")]
    pub completion_tokens: u64,
    /// 总共消耗的 token 数
    #[serde(default, deserialize_with = "null_to_zero")]
    pub total_tokens: u64,
    /// Prompt token 明细
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    /// 补全 token 明细
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
}

impl Usage {
    /// cache 命中的 prompt token 数
    pub fn cached_tokens(&self) -> u64 {
        self.prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0)
    }

    /// 思考占用的 token 数（非推理模型为 0）
    pub fn reasoning_tokens(&self) -> u64 {
        self.completion_tokens_details
            .as_ref()
            .and_then(|d| d.reasoning_tokens)
            .unwrap_or(0)
    }

    /// 扣除 cache 命中后的 prompt token 数
    ///
    /// `prompt_tokens` 通常包含 cache 命中的部分，直接用它反推 prefill 速度会明显偏高。
    pub fn uncached_prompt_tokens(&self) -> u64 {
        self.prompt_tokens.saturating_sub(self.cached_tokens())
    }
}

/// Token 生成速度统计（部分 API 的 `timings` 字段，如 vLLM）
///
/// # 参考示例
/// ```json
/// "timings": {
///   "prompt_n": 11,
///   "prompt_ms": 286.864,
///   "prompt_per_second": 38.34569691561158,
///   "predicted_n": 485,
///   "predicted_ms": 11568.86,
///   "predicted_per_second": 41.92288609249312
/// }
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Timings {
    /// cache 命中的 prompt token 数
    #[serde(default, deserialize_with = "null_to_zero")]
    pub cache_n: u64,
    /// Prompt 阶段消耗的 token 数
    #[serde(default, deserialize_with = "null_to_zero")]
    pub prompt_n: u64,
    /// Prompt 阶段耗时（毫秒）
    #[serde(default, deserialize_with = "null_to_zero_f64")]
    pub prompt_ms: f64,
    /// 每个 prompt token 平均耗时（毫秒）
    #[serde(default, deserialize_with = "null_to_zero_f64")]
    pub prompt_per_token_ms: f64,
    /// Prompt 阶段 token 速度（每秒 token 数）
    #[serde(default, deserialize_with = "null_to_zero_f64")]
    pub prompt_per_second: f64,
    /// 预测（补全）阶段生成的 token 数
    #[serde(default, deserialize_with = "null_to_zero")]
    pub predicted_n: u64,
    /// 预测（补全）阶段耗时（毫秒）
    #[serde(default, deserialize_with = "null_to_zero_f64")]
    pub predicted_ms: f64,
    /// 每个补全 token 平均耗时（毫秒）
    #[serde(default, deserialize_with = "null_to_zero_f64")]
    pub predicted_per_token_ms: f64,
    /// 预测（补全）阶段 token 速度（每秒 token 数）
    #[serde(default, deserialize_with = "null_to_zero_f64")]
    pub predicted_per_second: f64,
}

impl Timings {
    /// 补全阶段的 token 速度（每秒 token 数）
    ///
    /// 优先返回 `predicted_per_second`；为 0 时用 `predicted_n / predicted_ms` 反推。
    /// 0 / 负数 / 非有限值一律返回 `None`，交给调用方走退化路径。
    pub fn completion_tokens_per_second(&self) -> Option<f64> {
        self.finish(
            self.predicted_per_second,
            self.predicted_n as f64,
            self.predicted_ms,
        )
    }

    /// Prompt 阶段的 token 速度（每秒 token 数）
    pub fn prompt_tokens_per_second(&self) -> Option<f64> {
        self.finish(self.prompt_per_second, self.prompt_n as f64, self.prompt_ms)
    }

    /// 统一的「优先取现成值，否则按数量/耗时反推」逻辑
    fn finish(&self, provided: f64, count: f64, ms: f64) -> Option<f64> {
        let tps = if provided > 0.0 {
            provided
        } else if ms > 0.0 {
            count / (ms / 1000.0)
        } else {
            return None;
        };
        (tps > 0.0 && tps.is_finite()).then_some(tps)
    }
}

/// 非流式完整响应
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletion {
    /// 响应唯一标识
    #[serde(default)]
    pub id: Option<String>,
    /// 对象类型（`chat.completion`）
    #[serde(default)]
    pub object: Option<String>,
    /// 创建时间（Unix 秒）
    #[serde(default)]
    pub created: Option<i64>,
    /// 使用的模型名称
    #[serde(default)]
    pub model: Option<String>,
    /// 生成的候选结果列表
    #[serde(default)]
    pub choices: Vec<Choice>,
    /// Token 使用量统计
    #[serde(default)]
    pub usage: Option<Usage>,
    /// Token 生成速度统计（部分 API 如 vLLM 会返回）
    #[serde(default)]
    pub timings: Option<Timings>,
    /// 后端配置指纹
    #[serde(default)]
    pub system_fingerprint: Option<String>,
    /// 部分服务在 HTTP 200 时也把错误放在响应体里
    #[serde(default)]
    pub error: Option<Value>,
}

/// 单个生成结果
#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    /// 候选序号
    #[serde(default)]
    pub index: u32,
    /// 响应消息体
    #[serde(default)]
    pub message: ResponseMessage,
    /// 结束原因：stop / length / content_filter / tool_calls 等
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// 响应中的消息体
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResponseMessage {
    /// 消息角色
    #[serde(default)]
    pub role: Option<String>,
    /// 正文内容（兼容字符串与多模态数组两种形态）
    #[serde(default, deserialize_with = "deserialize_content_text")]
    pub content: Option<String>,
    /// 思考/推理内容（DeepSeek-reasoner、QwQ 等推理模型会返回此字段）
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// 思考/推理内容（部分网关使用此字段名）
    #[serde(default)]
    pub reasoning: Option<String>,
    /// 工具调用列表
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// 模型拒绝回答时的说明
    #[serde(default)]
    pub refusal: Option<String>,
}

impl ChatCompletion {
    /// 第一个候选
    pub fn first_choice(&self) -> Option<&Choice> {
        self.choices.first()
    }

    /// 指定序号的候选
    pub fn choice(&self, index: u32) -> Option<&Choice> {
        self.choices.iter().find(|c| c.index == index)
    }

    /// 第一个候选的回复正文（空字符串表示无内容）
    pub fn content(&self) -> &str {
        self.first_choice()
            .and_then(|c| c.message.content.as_deref())
            .unwrap_or("")
    }

    /// 第一个候选的思考内容
    ///
    /// 优先 `reasoning_content`，为空时回退到 `reasoning` 字段。
    pub fn reasoning_content(&self) -> Option<&str> {
        let message = &self.first_choice()?.message;
        message
            .reasoning_content
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| message.reasoning.as_deref().filter(|s| !s.is_empty()))
    }

    /// 第一个候选的工具调用
    pub fn tool_calls(&self) -> &[ToolCall] {
        self.first_choice()
            .and_then(|c| c.message.tool_calls.as_deref())
            .unwrap_or(&[])
    }

    /// 第一个候选的结束原因
    pub fn finish_reason(&self) -> Option<&str> {
        self.first_choice().and_then(|c| c.finish_reason.as_deref())
    }

    /// 是否因 `max_tokens` 被截断
    pub fn is_truncated(&self) -> bool {
        matches!(self.finish_reason(), Some("length"))
    }

    /// 补全阶段的 token 速度（每秒 token 数）
    ///
    /// 仅来自服务端 `timings`。没有 timings 时，需要调用方结合请求耗时与 usage 自行反推
    /// （见 [`crate::metrics::calc_tokens_per_second`]）。
    pub fn completion_tokens_per_second(&self) -> Option<f64> {
        self.timings
            .as_ref()
            .and_then(|t| t.completion_tokens_per_second())
    }

    /// Prompt 阶段的 token 速度（每秒 token 数）
    pub fn prompt_tokens_per_second(&self) -> Option<f64> {
        self.timings
            .as_ref()
            .and_then(|t| t.prompt_tokens_per_second())
    }
}

/// 正文反序列化：兼容 `"text"`、`[{...}]` 与 `null`
fn deserialize_content_text<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<Value>::deserialize(deserializer)?.and_then(value_to_text))
}

/// 把多模态数组里的文本片段拼成纯文本
fn value_to_text(value: Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    out.push_str(text);
                } else if let Some(text) = part.as_str() {
                    out.push_str(text);
                }
            }
            Some(out)
        }
        other => Some(other.to_string()),
    }
}

/// 模型列表响应
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelsResponse {
    /// 模型条目
    #[serde(default)]
    pub data: Vec<ModelInfo>,
}

/// 单个模型信息
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// 模型 ID（调用时使用的名称）
    pub id: String,
    /// 归属方
    #[serde(default)]
    pub owned_by: Option<String>,
    /// 创建时间（Unix 秒）
    #[serde(default)]
    pub created: Option<i64>,
}

/// 向量化响应
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmbeddingResponse {
    /// 使用的模型
    #[serde(default)]
    pub model: Option<String>,
    /// 向量数据
    #[serde(default)]
    pub data: Vec<Embedding>,
    /// Token 统计
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// 单条向量
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Embedding {
    /// 输入序号
    #[serde(default)]
    pub index: u32,
    /// 向量值
    #[serde(default)]
    pub embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 标准响应：正文、思考、usage、finish_reason 都要能取到
    #[test]
    fn test_completion_basic() {
        let json = r#"{
            "id": "test-123",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello world!",
                    "reasoning_content": "Let me think..."
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        }"#;
        let completion: ChatCompletion = serde_json::from_str(json).unwrap();
        assert_eq!(completion.content(), "Hello world!");
        assert_eq!(completion.reasoning_content(), Some("Let me think..."));
        assert_eq!(completion.finish_reason(), Some("stop"));
        assert_eq!(completion.usage.unwrap().total_tokens, 15);
    }

    /// `id` / `choices` 缺失、usage 字段为 null 时不应解析失败
    #[test]
    fn test_completion_tolerates_missing_fields() {
        let json = r#"{"choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":null}}"#;
        let completion: ChatCompletion = serde_json::from_str(json).unwrap();
        assert_eq!(completion.content(), "ok");
        assert_eq!(completion.usage.unwrap().prompt_tokens, 0);
    }

    /// 多模态内容的数组形态必须降级成文本，而不是让整个响应解析失败
    #[test]
    fn test_content_array_is_flattened() {
        let json = r#"{"choices":[{"message":{"content":[
            {"type":"text","text":"第一段"},
            {"type":"image_url","image_url":{"url":"https://x/y.png"}},
            {"type":"text","text":"第二段"}
        ]}}]}"#;
        let completion: ChatCompletion = serde_json::from_str(json).unwrap();
        assert_eq!(completion.content(), "第一段第二段");
    }

    /// 工具调用响应
    #[test]
    fn test_tool_calls_parsed() {
        let json = r#"{"choices":[{"message":{"content":null,"tool_calls":[
            {"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{}"}}
        ]},"finish_reason":"tool_calls"}]}"#;
        let completion: ChatCompletion = serde_json::from_str(json).unwrap();
        assert_eq!(completion.tool_calls().len(), 1);
        assert_eq!(completion.tool_calls()[0].function.name, "get_weather");
        assert_eq!(completion.finish_reason(), Some("tool_calls"));
    }

    /// usage 明细：cache 命中与思考 token
    #[test]
    fn test_usage_details() {
        let usage: Usage = serde_json::from_str(
            r#"{
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "total_tokens": 150,
                "prompt_tokens_details": {"cached_tokens": 80, "audio_tokens": null},
                "completion_tokens_details": {"reasoning_tokens": 30}
            }"#,
        )
        .unwrap();
        assert_eq!(usage.uncached_prompt_tokens(), 20);
        assert_eq!(usage.reasoning_tokens(), 30);
    }

    /// timings：优先使用现成速度，缺失时按数量/耗时反推
    #[test]
    fn test_timings_calculation() {
        let timings = Timings {
            prompt_n: 10,
            prompt_ms: 100.0,
            prompt_per_second: 100.0,
            predicted_n: 500,
            predicted_ms: 10_000.0,
            predicted_per_second: 50.0,
            ..Default::default()
        };
        assert!((timings.completion_tokens_per_second().unwrap() - 50.0).abs() < 1e-9);
        assert!((timings.prompt_tokens_per_second().unwrap() - 100.0).abs() < 1e-9);

        let derived = Timings {
            predicted_n: 100,
            predicted_ms: 2000.0,
            ..Default::default()
        };
        assert!((derived.completion_tokens_per_second().unwrap() - 50.0).abs() < 1e-9);
        assert!(Timings::default().completion_tokens_per_second().is_none());
    }

    /// timings 字段为 null 时按 0 处理
    #[test]
    fn test_timings_tolerates_null() {
        let timings: Timings =
            serde_json::from_str(r#"{"prompt_n":null,"prompt_per_second":null}"#).unwrap();
        assert_eq!(timings.prompt_n, 0);
        assert_eq!(timings.prompt_per_second, 0.0);
    }
}
