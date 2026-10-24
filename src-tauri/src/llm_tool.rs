//! # OpenAI 兼容大模型调用工具类
//!
//! 本模块实现了一个独立的 OpenAI 兼容大模型调用工具类，基于 [openaillm.md](../openaillm.md) 指导文档。
//!
//! ## 功能特性
//! - ✅ 非流式调用（一次性返回完整结果）
//! - ✅ 流式调用（SSE 逐 token 推送）
//! - ✅ 思考内容解析（`reasoning_content` 字段，兼容 DeepSeek / QwQ 等推理模型）
//! - ✅ 模型参数灵活配置（temperature / top_p / max_tokens / 自定义扩展参数等）
//! - ✅ 统一错误类型，覆盖 HTTP / JSON / API 业务 / 流解析四类异常
//!
//! ## 使用方式
//!
//! ```rust,no_run
//! use llmperf_lib::*;
//!
//! #[tokio::main]
//! async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
//!     // 1. 配置客户端
//!     let config = ClientConfig {
//!         base_url: "https://api.deepseek.com/v1".into(),
//!         api_key: "sk-your-api-key".into(),
//!         timeout_secs: 120,
//!     };
//!     let client = LlmClient::new(config)?;
//!
//!     // 2. 非流式调用
//!     let params = ChatParams { model: "deepseek-reasoner".into(), ..Default::default() };
//!     let messages = vec![
//!         ChatMessage::system("你是一个知识渊博的AI助手。"),
//!         ChatMessage::user("解释一下什么是量子纠缠？"),
//!     ];
//!     let response = client.chat(messages, &params).await?;
//!     println!("回复: {}", response.content());
//!
//!     Ok(())
//! }
//! ```

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::Client as ReqwestClient;
use serde::{Deserialize, Deserializer, Serialize};
use std::pin::Pin;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

// ============================ 错误类型 ============================

/// 统一错误类型，覆盖 HTTP / JSON / API 业务 / 流解析四类异常
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// HTTP 请求层错误（网络不通、超时、DNS 解析失败等）
    #[error("HTTP请求错误: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON 序列化 / 反序列化错误
    #[error("JSON错误: {0}")]
    Json(#[from] serde_json::Error),

    /// API 返回的业务错误（如鉴权失败、参数非法、余额不足等）
    #[error("API错误(HTTP {status}): {message}")]
    Api { status: u16, message: String },

    /// 流式 SSE 数据解析错误
    #[error("流解析错误: {0}")]
    StreamParse(String),
}

/// 统一结果类型别名
pub type Result<T> = std::result::Result<T, LlmError>;

// ============================ 客户端配置 ============================

/// API 客户端基础配置
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// API 基础地址，例如：
    /// - OpenAI:   https://api.openai.com/v1
    /// - DeepSeek: https://api.deepseek.com/v1
    /// - 通义千问: https://dashscope.aliyuncs.com/compatible-mode/v1
    pub base_url: String,

    /// API 密钥（Bearer Token）
    pub api_key: String,

    /// 请求超时时间（秒），流式场景建议设置较大值
    pub timeout_secs: u64,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            timeout_secs: 120,
        }
    }
}

// ============================ 消息模型 ============================

/// 消息角色
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 聊天消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// 消息角色
    pub role: Role,
    /// 消息正文
    pub content: String,
    /// 思考内容（多轮对话回传时使用，部分模型要求携带上一轮的推理过程）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl ChatMessage {
    /// 创建系统提示消息
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            reasoning_content: None,
        }
    }

    /// 创建用户消息
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            reasoning_content: None,
        }
    }

    /// 创建助手消息
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            reasoning_content: None,
        }
    }

    /// 创建带思考内容的助手消息（用于多轮对话回传推理链）
    pub fn assistant_with_reasoning(
        content: impl Into<String>,
        reasoning: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            reasoning_content: Some(reasoning.into()),
        }
    }
}

// ============================ 模型参数配置 ============================

/// 大模型调用参数（可按需配置，None 表示不传给 API 使用其默认值）
#[derive(Debug, Clone, Default, Serialize)]
pub struct ChatParams {
    /// 模型名称，如 "gpt-4o"、"deepseek-reasoner"、"qwen-plus"
    pub model: String,

    /// 采样温度 [0.0, 2.0]，值越大输出越随机、越有创造性
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// 核采样概率 [0.0, 1.0]，与 temperature 二选一使用
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    /// 最大生成 token 数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// 频率惩罚 [-2.0, 2.0]，正值降低重复词出现的概率
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,

    /// 存在惩罚 [-2.0, 2.0]，正值鼓励模型谈论新话题
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,

    /// 停止词列表，模型生成到这些词时停止
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,

    /// 生成候选数量（默认 1）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,

    /// 扩展参数：支持传入任意自定义字段（如 tools、response_format 等），
    /// 通过 serde flatten 直接合并到请求体顶层
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ============================ 响应模型（非流式） ============================

/// Prompt 阶段的 token 明细（部分 API 提供，用于区分 cache 命中）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptTokensDetails {
    /// cache 命中的 prompt token 数（命中部分通常不参与真实 prefill 计算）
    #[serde(default)]
    pub cached_tokens: u64,
}

/// 反序列化辅助：把显式 `null` 当成 0
///
/// `#[serde(default)]` 只能处理"字段缺失"，遇到显式 `null` 仍会整体报错，
/// 而部分厂商的流式 usage 确实会返回 null。
fn null_to_zero<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<u64>::deserialize(deserializer)?.unwrap_or(0))
}

/// Token 使用量统计
///
/// 所有字段都容忍缺失与 null：不同厂商的流式 usage 字段并不统一，
/// 缺少单个字段不应让整条流解析失败。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Prompt token 明细（cache 命中数），部分 API 返回
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

impl Usage {
    /// 扣除 cache 命中后的 prompt token 数
    ///
    /// `prompt_tokens` 通常包含 cache 命中的部分，直接用它反推 prefill 速度会明显偏高，
    /// 因此退化路径统一使用本方法返回的有效 token 数。
    pub fn uncached_prompt_tokens(&self) -> u64 {
        let cached = self
            .prompt_tokens_details
            .as_ref()
            .map(|d| d.cached_tokens)
            .unwrap_or(0);
        self.prompt_tokens.saturating_sub(cached)
    }
}

/// Token 生成速度统计（来自部分 API 的 `timings` 字段，如 vLLM）
///
/// # 参考示例
/// 参考1（vLLM）返回的 timings 字段：
/// ```json
/// "timings": {
///   "prompt_n": 11,
///   "prompt_ms": 286.864,
///   "prompt_per_token_ms": 26.078545454545452,
///   "prompt_per_second": 38.34569691561158,
///   "predicted_n": 485,
///   "predicted_ms": 11568.86,
///   "predicted_per_token_ms": 23.853319587628867,
///   "predicted_per_second": 41.92288609249312
/// }
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timings {
    /// Prompt 阶段消耗的 token 数
    #[serde(default)]
    pub prompt_n: u64,
    /// Prompt 阶段耗时（毫秒）
    #[serde(default)]
    pub prompt_ms: f64,
    /// 每个 prompt token 平均耗时（毫秒）
    #[serde(default)]
    pub prompt_per_token_ms: f64,
    /// Prompt 阶段 token 速度（每秒 token 数）
    #[serde(default)]
    pub prompt_per_second: f64,
    /// 预测（补全）阶段生成的 token 数
    #[serde(default)]
    pub predicted_n: u64,
    /// 预测（补全）阶段耗时（毫秒）
    #[serde(default)]
    pub predicted_ms: f64,
    /// 每个补全 token 平均耗时（毫秒）
    #[serde(default)]
    pub predicted_per_token_ms: f64,
    /// 预测（补全）阶段 token 速度（每秒 token 数）
    #[serde(default)]
    pub predicted_per_second: f64,
}

impl Timings {
    /// 获取补全阶段的 token 速度（每秒 token 数）
    ///
    /// 优先返回 `predicted_per_second`，若为空则尝试计算 `predicted_n / (predicted_ms / 1000.0)`。
    /// 计算结果为 0 / 负数 / 非有限值时一律返回 None，交由调用方走退化路径，
    /// 避免把无意义的 0 当成"有效数据"而跳过更可靠的 usage 反推。
    pub fn completion_tokens_per_second(&self) -> Option<f64> {
        let tps = if self.predicted_per_second > 0.0 {
            self.predicted_per_second
        } else if self.predicted_ms > 0.0 {
            self.predicted_n as f64 / (self.predicted_ms / 1000.0)
        } else {
            return None;
        };
        (tps > 0.0 && tps.is_finite()).then_some(tps)
    }

    /// 获取 Prompt 阶段的 token 速度（每秒 token 数）
    ///
    /// 与 `completion_tokens_per_second` 一致，非法值统一返回 None。
    pub fn prompt_tokens_per_second(&self) -> Option<f64> {
        let tps = if self.prompt_per_second > 0.0 {
            self.prompt_per_second
        } else if self.prompt_ms > 0.0 {
            self.prompt_n as f64 / (self.prompt_ms / 1000.0)
        } else {
            return None;
        };
        (tps > 0.0 && tps.is_finite()).then_some(tps)
    }
}

/// 非流式完整响应
#[derive(Debug, Deserialize)]
pub struct ChatCompletion {
    /// 响应唯一标识
    pub id: String,
    /// 使用的模型名称
    pub model: Option<String>,
    /// 生成的候选结果列表
    pub choices: Vec<Choice>,
    /// Token 使用量统计
    pub usage: Option<Usage>,
    /// Token 生成速度统计（部分 API 如 vLLM 会返回）
    #[serde(default)]
    pub timings: Option<Timings>,
}

/// 单个生成结果
#[derive(Debug, Deserialize)]
pub struct Choice {
    /// 候选序号
    pub index: u32,
    /// 响应消息体
    pub message: ResponseMessage,
    /// 结束原因：stop / length / content_filter / tool_calls 等
    pub finish_reason: Option<String>,
}

/// 响应中的消息体
#[derive(Debug, Deserialize)]
pub struct ResponseMessage {
    /// 消息角色
    pub role: Option<String>,
    /// 正文内容
    pub content: Option<String>,
    /// 思考/推理内容（DeepSeek-reasoner、QwQ 等推理模型会返回此字段）
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// 思考/推理内容（部分 API 使用此字段名，如 OpenAI o1/o3 系列）
    #[serde(default, alias = "reasoning")]
    pub reasoning: Option<String>,
}

impl ChatCompletion {
    /// 获取第一个 choice 的回复正文（空字符串表示无内容）
    pub fn content(&self) -> &str {
        self.choices
            .first()
            .and_then(|c| c.message.content.as_deref())
            .unwrap_or("")
    }

    /// 获取第一个 choice 的思考内容（若模型不支持则返回 None）
    ///
    /// 优先返回 `reasoning_content` 字段，若为空则尝试 `reasoning` 字段
    pub fn reasoning_content(&self) -> Option<&str> {
        self.choices
            .first()
            .and_then(|c| c.message.reasoning_content.as_deref())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.choices
                    .first()
                    .and_then(|c| c.message.reasoning.as_deref())
                    .filter(|s| !s.is_empty())
            })
    }

    /// 获取结束原因
    pub fn finish_reason(&self) -> Option<&str> {
        self.choices.first().and_then(|c| c.finish_reason.as_deref())
    }

    /// 获取补全阶段的 token 速度（每秒 token 数）
    ///
    /// 优先从 `timings.predicted_per_second` 获取，若为空则尝试从 `usage` 和响应时间计算。
    /// 注意：当前实现仅从 timings 获取，响应时间计算需要调用方传入请求耗时。
    pub fn completion_tokens_per_second(&self) -> Option<f64> {
        self.timings
            .as_ref()
            .and_then(|t| t.completion_tokens_per_second())
    }

    /// 获取 Prompt 阶段的 token 速度（每秒 token 数）
    pub fn prompt_tokens_per_second(&self) -> Option<f64> {
        self.timings
            .as_ref()
            .and_then(|t| t.prompt_tokens_per_second())
    }
}

// ============================ 流式响应模型 ============================

/// 流式响应中的单个 chunk（SSE data 字段反序列化结果）
#[derive(Debug, Deserialize)]
struct ChatChunk {
    #[allow(dead_code)]
    id: Option<String>,
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    /// 部分 API 在最后一个 chunk 中返回 usage
    #[serde(default)]
    usage: Option<Usage>,
    /// 部分 API（如 vLLM）在最后一个 chunk 中返回 timings
    #[serde(default)]
    timings: Option<Timings>,
}

#[derive(Debug, Default, Deserialize)]
struct ChunkChoice {
    /// 部分实现会在结束 chunk 中省略 delta，缺失时按空增量处理
    #[serde(default)]
    delta: DeltaMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

/// 流式增量消息
#[derive(Debug, Default, Deserialize)]
struct DeltaMessage {
    #[allow(dead_code)]
    role: Option<String>,
    /// 正文增量
    #[serde(default)]
    content: Option<String>,
    /// 思考内容增量（推理模型在思考阶段会持续输出此字段）
    #[serde(default)]
    reasoning_content: Option<String>,
    /// 思考内容增量（部分 API 使用此字段名，如 OpenAI o1/o3 系列）
    #[serde(default, alias = "reasoning")]
    reasoning: Option<String>,
}

/// 流式结束信息
#[derive(Debug, Clone)]
pub struct FinishInfo {
    /// 结束原因
    pub reason: String,
    /// Token 使用量（需要 API 支持 stream_options.include_usage）
    pub usage: Option<Usage>,
    /// Token 速度统计（vLLM 等 API 在最后一个 chunk 提供；优先于 usage + 时间戳反推）
    pub timings: Option<Timings>,
    /// 首个增量（思考或正文）到达时刻，由 SSE 解析器在字节到达时打点
    ///
    /// 用于估算 prefill 阶段耗时。注意不能取"首个正文"时刻：推理模型会先长时间输出
    /// 思考内容，用正文时刻会把思考生成时间误算进 prefill。
    pub first_token_at: Option<Instant>,
}

/// 流式事件枚举 —— 调用方通过匹配此枚举处理不同的流式数据
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// 思考/推理内容的增量文本
    ReasoningDelta(String),
    /// 正文回复的增量文本
    ContentDelta(String),
    /// 生成完成
    Finish(FinishInfo),
}

/// 流式结果聚合器：将零散的流式事件累积为完整结果
///
/// > 注：当前非流式接口已可直接获取完整内容，该结构体为后续流式结果聚合预留。
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct StreamAccumulator {
    /// 累积的思考内容
    pub reasoning_content: String,
    /// 累积的正文内容
    pub content: String,
    /// 结束原因
    pub finish_reason: Option<String>,
    /// Token 使用量
    pub usage: Option<Usage>,
}

impl StreamAccumulator {
    /// 喂入一个流式事件，自动累积到对应字段
    #[allow(dead_code)]
    pub fn feed(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::ReasoningDelta(s) => self.reasoning_content.push_str(s),
            StreamEvent::ContentDelta(s) => self.content.push_str(s),
            StreamEvent::Finish(info) => {
                self.finish_reason = Some(info.reason.clone());
                self.usage = info.usage.clone();
            }
        }
    }
}

// ============================ SSE 解析器 ============================

/// SSE 解析状态
enum ParseAction {
    /// 成功解析出一个事件
    Event(Result<StreamEvent>),
    /// 当前行无有效数据，跳过继续解析下一行
    Skip,
    /// 缓冲区数据不足，需要从流中读取更多数据
    NeedMore,
    /// 收到 [DONE] 标记，流结束
    StreamEnd,
    /// 解析成功但产出多条事件，仅返回首条；剩余事件应在下次解析时继续派发
    Events(Vec<StreamEvent>),
}

/// SSE 单行缓冲上限（字节）
///
/// 正常 SSE 每行几十到几百字节。若服务端长时间不发送换行（异常实现或恶意端点），
/// 缓冲区会无界增长直到 OOM，因此超过上限时主动中断并报错。
const MAX_SSE_BUFFER_BYTES: usize = 4 * 1024 * 1024;

/// SSE（Server-Sent Events）流解析器
/// 负责将原始字节流按行切割，解析 `data: {...}` 格式的 SSE 事件
struct SseParser {
    /// 底层字节流（来自 reqwest 的 response body stream）
    stream: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    /// 行缓冲区：处理跨 chunk 的不完整行
    buffer: Vec<u8>,
    /// 流是否已读取完毕
    stream_ended: bool,
    /// 同 chunk 解析出的剩余事件：当 `into_events` 产出多条事件（推理 + 正文），
    /// 首条立即返回，其余缓存在这里，按顺序在下一次 `next_event` 中派发。
    pending_events: Vec<StreamEvent>,
    /// 首个增量（思考或正文）到达时刻，随 Finish 事件带出，用于计算 prefill 耗时
    first_token_at: Option<Instant>,
}

impl SseParser {
    fn new(stream: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>) -> Self {
        Self {
            stream,
            buffer: Vec::with_capacity(4096),
            stream_ended: false,
            pending_events: Vec::new(),
            first_token_at: None,
        }
    }

    /// 异步获取下一个流式事件，返回 None 表示流结束
    async fn next_event(&mut self) -> Option<Result<StreamEvent>> {
        loop {
            // 0) 先派发上一轮缓存的同 chunk 后续事件（推理+正文共存场景）
            if !self.pending_events.is_empty() {
                return Some(Ok(self.pending_events.remove(0)));
            }

            // 第一步：尝试从缓冲区中解析出一个完整事件
            match self.try_parse_line() {
                ParseAction::Event(e) => return Some(e),
                ParseAction::Events(mut events) => {
                    // 首条立即返回，其余缓存到 pending_events
                    let first = events.remove(0);
                    self.pending_events = events;
                    return Some(Ok(first));
                }
                ParseAction::Skip => continue,       // 空行或无效行，继续解析
                ParseAction::StreamEnd => {
                    // 收到 [DONE]：补发一个"流正常结束"标记事件。
                    // 调用方据此区分"服务端确认结束"与"连接被中途截断"
                    // （后者不会产生任何 Finish 事件）。
                    self.stream_ended = true;
                    return Some(Ok(StreamEvent::Finish(FinishInfo {
                        reason: String::new(),
                        usage: None,
                        timings: None,
                        first_token_at: self.first_token_at,
                    })));
                }
                ParseAction::NeedMore => {}            // 需要更多数据，继续往下读
            }

            // 缓冲区中没有完整行，从底层流中读取更多数据
            if self.stream_ended {
                return None;
            }

            match self.stream.next().await {
                Some(Ok(bytes)) => {
                    self.buffer.extend_from_slice(&bytes);
                    // 防御异常/恶意端点：长时间不换行时缓冲会无界增长
                    if self.buffer.len() > MAX_SSE_BUFFER_BYTES {
                        self.stream_ended = true;
                        return Some(Err(LlmError::StreamParse(format!(
                            "SSE 单行数据超过 {} MiB，已中断",
                            MAX_SSE_BUFFER_BYTES / 1024 / 1024
                        ))));
                    }
                }
                Some(Err(e)) => {
                    self.stream_ended = true;
                    return Some(Err(LlmError::StreamParse(format!(
                        "读取流失败: {e}"
                    ))));
                }
                None => {
                    self.stream_ended = true;
                    // 流结束，最后尝试解析缓冲区中的剩余数据
                    match self.try_parse_line() {
                        ParseAction::Event(e) => return Some(e),
                        _ => return None,
                    }
                }
            }
        }
    }

    /// 从缓冲区中尝试解析一行 SSE 数据
    fn try_parse_line(&mut self) -> ParseAction {
        // 查找换行符位置（SSE 以 \n 分隔行）
        let newline_pos = match self.buffer.iter().position(|&b| b == b'\n') {
            Some(pos) => pos,
            None => return ParseAction::NeedMore,
        };

        // 提取一行并移除缓冲区中已消费的数据
        let line_bytes: Vec<u8> = self.buffer.drain(..=newline_pos).collect();
        let line = String::from_utf8_lossy(&line_bytes);
        let line = line.trim_end_matches(['\n', '\r']);

        // 空行：SSE 事件分隔符，跳过
        if line.trim().is_empty() {
            return ParseAction::Skip;
        }

        // 只处理 "data:" 开头的行（忽略 event:、id:、retry: 等字段）
        if !line.starts_with("data:") {
            return ParseAction::Skip;
        }

        let data = line.trim_start_matches("data:").trim();

        // OpenAI 协议结束标记
        if data == "[DONE]" {
            return ParseAction::StreamEnd;
        }

        // 解析 JSON chunk 并转换为流式事件
        // 一个 chunk 可能产出 0~N 个事件（推理 + 正文 + finish），需逐条发送。
        match serde_json::from_str::<ChatChunk>(data) {
            Ok(chunk) => {
                let mut events = chunk.into_events();
                // 在解析层给事件打时间戳（比消费端更接近字节真实到达时刻）
                self.stamp_first_token(&mut events);
                match events.len() {
                    0 => ParseAction::Skip, // chunk 中无有意义的数据
                    1 => ParseAction::Event(Ok(events.remove(0))),
                    _ => ParseAction::Events(events),
                }
            }
            Err(e) => ParseAction::Event(Err(LlmError::StreamParse(format!(
                "JSON解析失败: {e}, 原始数据: {data}"
            )))),
        }
    }

    /// 给事件打时间戳：记录首个增量到达时刻，并回填到 Finish 事件
    ///
    /// 在 SSE 解析层打点比在消费端（channel 出队、跨任务调度之后）更接近
    /// 字节真正到达的时刻，高并发压测时差异尤其明显。
    fn stamp_first_token(&mut self, events: &mut [StreamEvent]) {
        for event in events.iter_mut() {
            match event {
                StreamEvent::ReasoningDelta(_) | StreamEvent::ContentDelta(_) => {
                    if self.first_token_at.is_none() {
                        self.first_token_at = Some(Instant::now());
                    }
                }
                StreamEvent::Finish(info) => info.first_token_at = self.first_token_at,
            }
        }
    }
}

impl ChatChunk {
    /// 将原始 chunk 转换为语义化的流式事件
    ///
    /// 单个 chunk 可能同时携带思考增量与正文增量（例如推理模型思考刚结束的那一刻），
    /// 因此返回 `Vec` 而非 `Option`：调用方按顺序派发多条事件即可保证两路内容都不丢失。
    fn into_events(self) -> Vec<StreamEvent> {
        let mut events = Vec::with_capacity(2);

        if let Some(choice) = self.choices.first() {
            let delta = &choice.delta;

            // 优先处理思考内容增量（推理模型在思考阶段只输出此字段）
            if let Some(reasoning) = &delta.reasoning_content {
                if !reasoning.is_empty() {
                    events.push(StreamEvent::ReasoningDelta(reasoning.clone()));
                }
            } else if let Some(reasoning) = &delta.reasoning {
                // 兜底：部分 API 使用 `reasoning` 而非 `reasoning_content`
                // 仅在首选字段缺失时使用，避免同一 chunk 重复派发。
                if !reasoning.is_empty() {
                    events.push(StreamEvent::ReasoningDelta(reasoning.clone()));
                }
            }

            // 正文内容增量（与上方思考增量互不冲突，可同时派发）
            if let Some(content) = &delta.content {
                if !content.is_empty() {
                    events.push(StreamEvent::ContentDelta(content.clone()));
                }
            }

            // 结束标记（附带 usage / timings 信息）
            if let Some(finish_reason) = &choice.finish_reason {
                events.push(StreamEvent::Finish(FinishInfo {
                    reason: finish_reason.clone(),
                    usage: self.usage.clone(),
                    timings: self.timings.clone(),
                    first_token_at: None, // 由 SseParser 打点后回填
                }));
            }
        }

        // 某些 API 的最后一个 chunk 只包含 usage / timings 而 choices 为空。
        // 只要两者任一存在就必须产出 Finish 事件，否则 timings 会被丢弃。
        if events.is_empty() && (self.usage.is_some() || self.timings.is_some()) {
            events.push(StreamEvent::Finish(FinishInfo {
                reason: String::new(),
                usage: self.usage,
                timings: self.timings,
                first_token_at: None, // 由 SseParser 打点后回填
            }));
        }

        events
    }
}

// ============================ 核心客户端 ============================

/// 大模型 API 客户端
///
/// 封装了非流式和流式两种调用方式，内部使用 reqwest 发送请求，
/// 通过 SSE 解析器处理流式响应。
///
/// 克隆开销极小：`reqwest::Client` 内部共享连接池，仅做引用计数。
#[derive(Clone)]
pub struct LlmClient {
    /// 底层 HTTP 客户端（连接池复用）
    http: ReqwestClient,
    /// 客户端配置
    config: ClientConfig,
}

impl LlmClient {
    /// 创建客户端实例
    pub fn new(config: ClientConfig) -> Result<Self> {
        let http = ReqwestClient::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()?;
        Ok(Self { http, config })
    }

    /// 构建请求体 JSON：合并模型参数 + 消息列表 + 流式标记
    fn build_request_body(
        &self,
        messages: &[ChatMessage],
        params: &ChatParams,
        stream: bool,
    ) -> Result<serde_json::Value> {
        // 将 ChatParams 序列化为 JSON（extra 字段会被 flatten 到顶层）
        let mut body = serde_json::to_value(params)?;

        if let Some(obj) = body.as_object_mut() {
            obj.insert("messages".to_string(), serde_json::to_value(messages)?);
            obj.insert("stream".to_string(), serde_json::Value::Bool(stream));

            // 流式模式下请求返回 usage 统计（OpenAI 兼容接口通用参数）
            if stream {
                obj.entry("stream_options")
                    .or_insert_with(|| serde_json::json!({ "include_usage": true }));
            }
        }

        Ok(body)
    }

    /// 发送 HTTP 请求并校验响应状态码
    async fn send_request(&self, body: &serde_json::Value) -> Result<reqwest::Response> {
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.config.base_url))
            .bearer_auth(&self.config.api_key)
            .json(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api { status, message });
        }

        Ok(resp)
    }

    /// **非流式调用**：一次性等待模型生成完毕，返回完整结果
    ///
    /// # 参数
    /// - `messages`: 对话消息列表
    /// - `params`: 模型参数配置
    ///
    /// # 返回
    /// - `ChatCompletion`: 包含正文内容、思考内容、usage 等完整信息
    pub async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        params: &ChatParams,
    ) -> Result<ChatCompletion> {
        let body = self.build_request_body(&messages, params, false)?;
        let resp = self.send_request(&body).await?;
        let completion: ChatCompletion = resp.json().await?;
        Ok(completion)
    }

    /// **流式调用**：通过 SSE 逐 token 接收模型输出
    ///
    /// # 参数
    /// - `messages`: 对话消息列表
    /// - `params`: 模型参数配置
    ///
    /// # 返回
    /// - `mpsc::Receiver<Result<StreamEvent>>`: 事件接收端，循环 recv 即可获取增量数据
    ///
    /// # 使用方式
    /// ```rust,ignore
    /// use llmperf_lib::*;
    ///
    /// # async fn example(client: &LlmClient, messages: Vec<ChatMessage>, params: &ChatParams) -> Result<()> {
    /// let mut rx = client.chat_stream(messages, &params).await?;
    /// while let Some(event) = rx.recv().await {
    ///     match event? {
    ///         StreamEvent::ReasoningDelta(s) => print!("💭 {s}"),
    ///         StreamEvent::ContentDelta(s) => print!("{s}"),
    ///         StreamEvent::Finish(info) => println!("\n✅ 完成: {}", info.reason),
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        params: &ChatParams,
    ) -> Result<mpsc::Receiver<Result<StreamEvent>>> {
        let body = self.build_request_body(&messages, params, true)?;
        let resp = self.send_request(&body).await?;

        // 获取响应体的字节流并装箱为 trait object
        let byte_stream: Pin<
            Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>,
        > = Box::pin(resp.bytes_stream());

        // 使用 channel 在后台任务中解析 SSE，主线程通过 Receiver 消费事件
        let (tx, rx) = mpsc::channel::<Result<StreamEvent>>(64);

        tokio::spawn(async move {
            let mut parser = SseParser::new(byte_stream);
            while let Some(event) = parser.next_event().await {
                // 如果接收端已关闭（如调用方 break），停止发送
                if tx.send(event).await.is_err() {
                    break;
                }
            }
        });

        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：服务端持续返回不含换行的数据时，行缓冲必须有上限，不能无界增长
    #[tokio::test]
    async fn test_sse_parser_rejects_oversized_line() {
        // 每次 1 MiB 且不含 \n，累计超过上限时应当中断
        let chunks: Vec<std::result::Result<Bytes, reqwest::Error>> = (0..8)
            .map(|_| Ok(Bytes::from(vec![b'a'; 1024 * 1024])))
            .collect();
        let stream: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>> =
            Box::pin(futures_util::stream::iter(chunks));

        let mut parser = SseParser::new(stream);
        let result = parser.next_event().await;

        assert!(
            matches!(result, Some(Err(LlmError::StreamParse(_)))),
            "超过缓冲上限应返回 StreamParse 错误"
        );
    }

    #[test]
    fn test_chat_message_creation() {
        let sys = ChatMessage::system("You are a helpful assistant.");
        assert_eq!(sys.role, Role::System);
        assert_eq!(sys.content, "You are a helpful assistant.");
        assert!(sys.reasoning_content.is_none());

        let user = ChatMessage::user("Hello!");
        assert_eq!(user.role, Role::User);

        let assistant = ChatMessage::assistant("Hi there!");
        assert_eq!(assistant.role, Role::Assistant);

        let with_reasoning = ChatMessage::assistant_with_reasoning(
            "结论",
            "因为...所以...",
        );
        assert_eq!(
            with_reasoning.reasoning_content,
            Some("因为...所以...".to_string())
        );
    }

    #[test]
    fn test_chat_params_default() {
        let params = ChatParams::default();
        assert!(params.temperature.is_none());
        assert!(params.top_p.is_none());
        assert!(params.max_tokens.is_none());
    }

    #[test]
    fn test_chat_completion_content_extraction() {
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
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        }"#;

        let completion: ChatCompletion = serde_json::from_str(json).unwrap();
        assert_eq!(completion.content(), "Hello world!");
        assert_eq!(
            completion.reasoning_content(),
            Some("Let me think...")
        );
        assert_eq!(completion.finish_reason(), Some("stop"));
        assert_eq!(completion.usage.unwrap().total_tokens, 15);
    }

    /// 测试 OpenAI o1/o3 系列模型返回的 reasoning 字段（而非 reasoning_content）
    #[test]
    fn test_chat_completion_reasoning_field() {
        let json = r#"{
            "id": "chatcmpl-9d2c5c11edc1e071",
            "object": "chat.completion",
            "created": 1786084098,
            "model": "LLM-AI-HEAT",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "\n\n你好！有什么我可以帮你的吗？",
                    "reasoning": "Here's a thinking process:\n\n1. **Analyze User Input:**\n   - User said: \"你好\" (Hello in Chinese)"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 11,
                "completion_tokens": 212,
                "total_tokens": 223
            }
        }"#;

        let completion: ChatCompletion = serde_json::from_str(json).unwrap();
        assert_eq!(completion.content(), "\n\n你好！有什么我可以帮你的吗？");
        // 应该能从 reasoning 字段提取思考内容
        assert!(
            completion.reasoning_content().is_some(),
            "应该能从 reasoning 字段提取思考内容"
        );
        assert!(
            completion.reasoning_content().unwrap().starts_with("Here's a thinking process"),
            "思考内容应该以 'Here's a thinking process' 开头"
        );
        assert_eq!(completion.finish_reason(), Some("stop"));
        assert_eq!(completion.usage.unwrap().total_tokens, 223);
    }

    #[test]
    fn test_stream_accumulator() {
        let mut acc = StreamAccumulator::default();

        acc.feed(&StreamEvent::ReasoningDelta("思考中".to_string()));
        acc.feed(&StreamEvent::ContentDelta("你好".to_string()));
        acc.feed(&StreamEvent::ContentDelta("世界".to_string()));
        acc.feed(&StreamEvent::Finish(FinishInfo {
            reason: "stop".to_string(),
            usage: None,
            timings: None,
            first_token_at: None,
        }));

        assert_eq!(acc.reasoning_content, "思考中");
        assert_eq!(acc.content, "你好世界");
        assert_eq!(acc.finish_reason, Some("stop".to_string()));
    }

    /// 测试流式 chunk 使用 `reasoning`（而非 `reasoning_content`）字段时能被识别为思考增量
    #[test]
    fn test_chat_chunk_reasoning_alias() {
        let json = r#"{
            "id": "chatcmpl-stream-1",
            "object": "chat.completion.chunk",
            "created": 1786084098,
            "model": "LLM-AI-HEAT",
            "choices": [{
                "index": 0,
                "delta": {
                    "reasoning": "Here's a thinking process:\n1. ..."
                }
            }]
        }"#;

        let chunk: ChatChunk = serde_json::from_str(json).unwrap();
        let events = chunk.into_events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ReasoningDelta(s) => {
                assert!(s.starts_with("Here's a thinking process"));
            }
            other => panic!("应解析为 ReasoningDelta，实际: {other:?}"),
        }
    }

    /// BUG A 回归测试：同一 chunk 中 `reasoning_content` 与 `content` 同时存在时，
    /// 两条内容都必须产出事件，不能因早返回而丢弃 `content`。
    #[test]
    fn test_chat_chunk_reasoning_and_content_coexist() {
        let json = r#"{
            "id": "chatcmpl-stream-2",
            "object": "chat.completion.chunk",
            "created": 1786084098,
            "model": "LLM-AI-HEAT",
            "choices": [{
                "index": 0,
                "delta": {
                    "reasoning_content": "尾句思考",
                    "content": "正文第一段"
                }
            }]
        }"#;

        let chunk: ChatChunk = serde_json::from_str(json).unwrap();
        let events = chunk.into_events();
        assert_eq!(events.len(), 2, "应当同时派发 reasoning + content 两条事件");

        let mut saw_reasoning = false;
        let mut saw_content = false;
        for ev in &events {
            match ev {
                StreamEvent::ReasoningDelta(s) if s == "尾句思考" => saw_reasoning = true,
                StreamEvent::ContentDelta(s) if s == "正文第一段" => saw_content = true,
                _ => {}
            }
        }
        assert!(saw_reasoning, "应产生 ReasoningDelta(尾句思考)");
        assert!(saw_content, "应产生 ContentDelta(正文第一段)");
    }

    /// 同 chunk 同时出现 `reasoning_content` 和 `reasoning` 时，业务层只取首个非空值
    /// （`reasoning_content` 优先于 `reasoning` 别名），避免重复派发。
    /// 真实场景下 API 不会同时给出两个相同语义的字段，本测试记录此行为约束。
    #[test]
    fn test_chat_chunk_reasoning_and_alias_dedup() {
        let json = r#"{
            "id": "chatcmpl-stream-3",
            "object": "chat.completion.chunk",
            "created": 1786084098,
            "model": "LLM-AI-HEAT",
            "choices": [{
                "index": 0,
                "delta": {
                    "reasoning_content": "权威字段",
                    "reasoning": "兜底字段"
                }
            }]
        }"#;

        let chunk: ChatChunk = serde_json::from_str(json).unwrap();
        let events = chunk.into_events();
        // 业务策略：reasoning_content 优先，alias 仅在首选缺失时兜底。
        assert_eq!(events.len(), 1, "同一字段不应被派发两次");
        match &events[0] {
            StreamEvent::ReasoningDelta(s) => assert_eq!(s, "权威字段"),
            other => panic!("应为 ReasoningDelta，实际: {other:?}"),
        }
    }

    #[test]
    fn test_client_config_default() {
        let config = ClientConfig::default();
        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert_eq!(config.timeout_secs, 120);
    }

    /// 测试带 timings 字段的响应解析（参考1：vLLM）
    #[test]
    fn test_chat_completion_with_timings() {
        let json = r#"{
            "id": "chatcmpl-6qSiIfas6wiYClQeG0o4AuGedbcHVfAe",
            "object": "chat.completion",
            "created": 1786156635,
            "model": "myllm",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "你好！我是通义千问...",
                    "reasoning_content": "Thinking Process:\n\n1. Analyze..."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "completion_tokens": 485,
                "prompt_tokens": 11,
                "total_tokens": 496,
                "prompt_tokens_details": {
                    "cached_tokens": 0
                }
            },
            "timings": {
                "cache_n": 0,
                "prompt_n": 11,
                "prompt_ms": 286.864,
                "prompt_per_token_ms": 26.078545454545452,
                "prompt_per_second": 38.34569691561158,
                "predicted_n": 485,
                "predicted_ms": 11568.86,
                "predicted_per_token_ms": 23.853319587628867,
                "predicted_per_second": 41.92288609249312
            }
        }"#;

        let completion: ChatCompletion = serde_json::from_str(json).unwrap();
        assert_eq!(completion.content(), "你好！我是通义千问...");
        assert!(completion.timings.is_some());

        let timings = completion.timings.as_ref().unwrap();
        assert_eq!(timings.prompt_n, 11);
        assert_eq!(timings.predicted_n, 485);
        assert!((timings.prompt_per_second - 38.34569691561158).abs() < 0.0001);
        assert!((timings.predicted_per_second - 41.92288609249312).abs() < 0.0001);

        // 测试 token 速度获取
        assert!(completion.completion_tokens_per_second().is_some());
        assert!((completion.completion_tokens_per_second().unwrap() - 41.92288609249312).abs() < 0.0001);
        assert!(completion.prompt_tokens_per_second().is_some());
        assert!((completion.prompt_tokens_per_second().unwrap() - 38.34569691561158).abs() < 0.0001);
    }

    /// 测试不带 timings 字段的响应解析（参考2：无 timings 的 vLLM）
    #[test]
    fn test_chat_completion_without_timings() {
        let json = r#"{
            "id": "chatcmpl-9d2c5c11edc1e071",
            "object": "chat.completion",
            "created": 1786084098,
            "model": "LLM-AI-HEAT",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "\n\n你好！有什么我可以帮你的吗？",
                    "reasoning": "Here's a thinking process..."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 11,
                "total_tokens": 223,
                "completion_tokens": 212,
                "prompt_tokens_details": null
            }
        }"#;

        let completion: ChatCompletion = serde_json::from_str(json).unwrap();
        assert_eq!(completion.content(), "\n\n你好！有什么我可以帮你的吗？");
        // 没有 timings 字段，应该默认为 None
        assert!(completion.timings.is_none());
        assert!(completion.completion_tokens_per_second().is_none());
        assert!(completion.prompt_tokens_per_second().is_none());
    }

    /// 测试 Timings 结构体的 token 速度计算方法
    #[test]
    fn test_timings_calculation() {
        // 有 predicted_per_second 时直接返回
        let timings = Timings {
            prompt_n: 10,
            prompt_ms: 100.0,
            prompt_per_token_ms: 10.0,
            prompt_per_second: 100.0,
            predicted_n: 500,
            predicted_ms: 10000.0,
            predicted_per_token_ms: 20.0,
            predicted_per_second: 50.0,
        };
        assert!((timings.completion_tokens_per_second().unwrap() - 50.0).abs() < 0.0001);
        assert!((timings.prompt_tokens_per_second().unwrap() - 100.0).abs() < 0.0001);

        // 没有 predicted_per_second 时从 predicted_n / (predicted_ms / 1000) 计算
        let timings_no_per_second = Timings {
            prompt_n: 0,
            prompt_ms: 0.0,
            prompt_per_token_ms: 0.0,
            prompt_per_second: 0.0,
            predicted_n: 100,
            predicted_ms: 2000.0,
            predicted_per_token_ms: 20.0,
            predicted_per_second: 0.0,
        };
        // 100 tokens / 2 seconds = 50 tokens/s
        assert!((timings_no_per_second.completion_tokens_per_second().unwrap() - 50.0).abs() < 0.0001);
    }

    /// 边界：timings 中的非法值（0 / 除数过小）必须返回 None，交由退化路径处理
    #[test]
    fn test_timings_ignores_zero_and_invalid() {
        // 全 0 timings
        assert!(Timings::default().completion_tokens_per_second().is_none());
        assert!(Timings::default().prompt_tokens_per_second().is_none());

        // predicted_ms > 0 但 predicted_n = 0 → 速度为 0，应视为无效
        let zero_tokens = Timings {
            predicted_n: 0,
            predicted_ms: 100.0,
            ..Default::default()
        };
        assert!(zero_tokens.completion_tokens_per_second().is_none());
    }

    /// usage 字段缺失 / 为 null 时不应让整条流解析失败（不同厂商实现差异很大）
    #[test]
    fn test_usage_tolerates_missing_fields() {
        let chunk: ChatChunk = serde_json::from_str(
            r#"{"choices":[],"usage":{"completion_tokens":42}}"#,
        )
        .expect("缺失字段的 usage 应能反序列化");
        let usage = chunk.usage.expect("usage 应存在");
        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 42);
        assert_eq!(usage.uncached_prompt_tokens(), 0);

        let chunk_null: ChatChunk = serde_json::from_str(
            r#"{"choices":[],"usage":{"prompt_tokens":null,"completion_tokens":null}}"#,
        )
        .expect("null 字段应能按默认值解析");
        let usage_null = chunk_null.usage.expect("usage 应存在");
        assert_eq!(usage_null.completion_tokens, 0);
    }

    /// 结束 chunk 省略 delta 时不应报错（部分实现在 finish chunk 里只给 finish_reason）
    #[test]
    fn test_chunk_without_delta_is_tolerated() {
        let chunk: ChatChunk = serde_json::from_str(
            r#"{"choices":[{"index":0,"finish_reason":"stop"}]}"#,
        )
        .expect("缺少 delta 的 chunk 应能反序列化");
        let events = chunk.into_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::Finish(_)));
    }

    /// 只有 timings、没有 usage 的尾 chunk 也必须产出 Finish，否则 timings 会被丢弃
    #[test]
    fn test_timings_only_chunk_emits_finish() {
        let chunk: ChatChunk = serde_json::from_str(
            r#"{"choices":[],"timings":{"prompt_n":4,"prompt_ms":100,"prompt_per_second":40,"predicted_n":42,"predicted_ms":1000,"predicted_per_second":42}}"#,
        )
        .expect("timings-only chunk 应能反序列化");
        let events = chunk.into_events();
        assert_eq!(events.len(), 1, "timings-only chunk 必须产出 Finish 事件");
        match &events[0] {
            StreamEvent::Finish(info) => {
                assert!(info.usage.is_none());
                let timings = info.timings.as_ref().expect("timings 不应丢失");
                assert!((timings.predicted_per_second - 42.0).abs() < 0.001);
            }
            other => panic!("预期 Finish 事件，实际为 {other:?}"),
        }
    }

    /// 端到端模拟 SSE 字节流：
    /// 1. 思考增量先于正文增量到达，首个增量必须是 ReasoningDelta
    /// 2. timings 与 usage 分散在两个 chunk 中，两者都不能丢
    /// 3. [DONE] 必须补发一个 Finish 事件（用于区分正常结束与中途截断）
    #[tokio::test]
    async fn test_sse_parser_stamps_first_token_and_keeps_timings() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"嗯\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}],\"timings\":{\"prompt_n\":4,\"prompt_ms\":100,\"prompt_per_second\":40,\"predicted_n\":10,\"predicted_ms\":500,\"predicted_per_second\":20}}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":6,\"completion_tokens\":10,\"total_tokens\":16}}\n\n",
            "data: [DONE]\n\n",
        );
        let bytes = Bytes::copy_from_slice(sse.as_bytes());
        let stream: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>> =
            Box::pin(futures_util::stream::iter(vec![Ok(bytes)]));

        let mut parser = SseParser::new(stream);
        let mut events = Vec::new();
        while let Some(event) = parser.next_event().await {
            events.push(event.expect("SSE 解析不应失败"));
        }

        // 事件顺序：思考 → 正文 → Finish(timings) → Finish(usage) → Finish([DONE])
        assert!(matches!(events[0], StreamEvent::ReasoningDelta(_)));
        assert!(matches!(events[1], StreamEvent::ContentDelta(_)));

        let finishes: Vec<&FinishInfo> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Finish(info) => Some(info),
                _ => None,
            })
            .collect();
        assert_eq!(finishes.len(), 3, "应包含 finish/usage/[DONE] 三个 Finish 事件");

        // 首个增量（思考）时刻必须被打点，且早于 Finish 到达
        let first_token_at = finishes[0].first_token_at.expect("首 token 时刻应被打点");
        assert!(finishes[0].timings.is_some(), "timings 不应丢失");
        assert!(finishes[0].usage.is_none());
        assert!(finishes[1].usage.is_some());
        assert!(finishes[1].timings.is_none());
        assert_eq!(finishes[1].first_token_at, Some(first_token_at));
        // [DONE] 补发的 Finish：三字段全空，仅表示"流正常结束"
        assert!(finishes[2].usage.is_none() && finishes[2].timings.is_none());
    }
}
