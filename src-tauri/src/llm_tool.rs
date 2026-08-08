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
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::time::Duration;
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

/// Token 使用量统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt 阶段消耗的 token 数
    pub prompt_tokens: u64,
    /// 补全阶段消耗的 token 数
    pub completion_tokens: u64,
    /// 总共消耗的 token 数
    pub total_tokens: u64,
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
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    delta: DeltaMessage,
    finish_reason: Option<String>,
}

/// 流式增量消息
#[derive(Debug, Deserialize)]
struct DeltaMessage {
    #[allow(dead_code)]
    role: Option<String>,
    /// 正文增量
    #[serde(default)]
    content: Option<String>,
    /// 思考内容增量（推理模型在思考阶段会持续输出此字段）
    #[serde(default)]
    reasoning_content: Option<String>,
}

/// 流式结束信息
#[derive(Debug, Clone)]
pub struct FinishInfo {
    /// 结束原因
    pub reason: String,
    /// Token 使用量（需要 API 支持 stream_options.include_usage）
    pub usage: Option<Usage>,
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
}

/// SSE（Server-Sent Events）流解析器
/// 负责将原始字节流按行切割，解析 `data: {...}` 格式的 SSE 事件
struct SseParser {
    /// 底层字节流（来自 reqwest 的 response body stream）
    stream: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    /// 行缓冲区：处理跨 chunk 的不完整行
    buffer: Vec<u8>,
    /// 流是否已读取完毕
    stream_ended: bool,
}

impl SseParser {
    fn new(stream: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>) -> Self {
        Self {
            stream,
            buffer: Vec::with_capacity(4096),
            stream_ended: false,
        }
    }

    /// 异步获取下一个流式事件，返回 None 表示流结束
    async fn next_event(&mut self) -> Option<Result<StreamEvent>> {
        loop {
            // 第一步：尝试从缓冲区中解析出一个完整事件
            match self.try_parse_line() {
                ParseAction::Event(e) => return Some(e),
                ParseAction::Skip => continue,       // 空行或无效行，继续解析
                ParseAction::StreamEnd => return None, // [DONE] 标记
                ParseAction::NeedMore => {}            // 需要更多数据，继续往下读
            }

            // 缓冲区中没有完整行，从底层流中读取更多数据
            if self.stream_ended {
                return None;
            }

            match self.stream.next().await {
                Some(Ok(bytes)) => {
                    self.buffer.extend_from_slice(&bytes);
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
        match serde_json::from_str::<ChatChunk>(data) {
            Ok(chunk) => match chunk.into_event() {
                Some(event) => ParseAction::Event(Ok(event)),
                None => ParseAction::Skip, // chunk 中无有意义的数据
            },
            Err(e) => ParseAction::Event(Err(LlmError::StreamParse(format!(
                "JSON解析失败: {e}, 原始数据: {data}"
            )))),
        }
    }
}

impl ChatChunk {
    /// 将原始 chunk 转换为语义化的流式事件
    fn into_event(self) -> Option<StreamEvent> {
        if let Some(choice) = self.choices.first() {
            let delta = &choice.delta;

            // 优先处理思考内容增量（推理模型在思考阶段只输出此字段）
            if let Some(reasoning) = &delta.reasoning_content {
                if !reasoning.is_empty() {
                    return Some(StreamEvent::ReasoningDelta(reasoning.clone()));
                }
            }

            // 正文内容增量
            if let Some(content) = &delta.content {
                if !content.is_empty() {
                    return Some(StreamEvent::ContentDelta(content.clone()));
                }
            }

            // 结束标记（附带 usage 信息）
            if let Some(finish_reason) = &choice.finish_reason {
                return Some(StreamEvent::Finish(FinishInfo {
                    reason: finish_reason.clone(),
                    usage: self.usage.clone(),
                }));
            }
        }

        // 某些 API 的最后一个 chunk 只包含 usage 而 choices 为空
        if let Some(usage) = self.usage {
            return Some(StreamEvent::Finish(FinishInfo {
                reason: String::new(),
                usage: Some(usage),
            }));
        }

        None
    }
}

// ============================ 核心客户端 ============================

/// 大模型 API 客户端
///
/// 封装了非流式和流式两种调用方式，内部使用 reqwest 发送请求，
/// 通过 SSE 解析器处理流式响应。
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
        }));

        assert_eq!(acc.reasoning_content, "思考中");
        assert_eq!(acc.content, "你好世界");
        assert_eq!(acc.finish_reason, Some("stop".to_string()));
    }

    #[test]
    fn test_client_config_default() {
        let config = ClientConfig::default();
        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert_eq!(config.timeout_secs, 120);
    }
}
