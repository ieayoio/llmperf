//! # llm-client
//!
//! OpenAI 兼容的大模型客户端，面向「可复用」设计：
//! 不依赖任何 GUI 框架，只依赖 `reqwest` / `tokio` / `serde`。
//!
//! ## 能力
//! - 非流式与流式（SSE）调用，流对象实现 [`futures_util::Stream`]
//! - 思考内容解析（`reasoning_content` / `reasoning`）
//! - 工具调用（function calling）：请求定义 + 流式增量累积
//! - 多模态内容（文本 / 图片）
//! - 多候选（`n > 1`，事件带 `index`）
//! - 请求取消（[`tokio_util::sync::CancellationToken`] 或丢弃流）
//! - 分级超时（连接 / 等待响应头 / 流空闲）、重试与退避、限长读取
//! - 结构化错误（状态码 / 业务码 / Retry-After）
//! - 压测指标工具（TTFT / TPOT / ITL / token 速度）
//!
//! ## 快速开始
//!
//! ```rust,no_run
//! use llm_client::{ChatMessage, ChatParams, ClientConfig, LlmClient, StreamEvent};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let client = LlmClient::new(ClientConfig::new("https://api.openai.com/v1", "sk-xxx"))?;
//! let params = ChatParams::new("gpt-4o").with_temperature(0.2);
//!
//! // 非流式
//! let completion = client.chat(vec![ChatMessage::user("你好")], &params).await?;
//! println!("{}", completion.content());
//!
//! // 流式
//! let mut stream = client.chat_stream(vec![ChatMessage::user("你好")], &params).await?;
//! while let Some(event) = stream.recv().await {
//!     match event? {
//!         StreamEvent::ContentDelta { text, .. } => print!("{text}"),
//!         StreamEvent::Finish(info) => println!("\n完成: {}", info.reason),
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## 工具调用
//!
//! ```rust,no_run
//! use llm_client::{ChatMessage, ChatParams, ClientConfig, LlmClient, Tool, ToolChoice};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let client = LlmClient::new(ClientConfig::new("https://api.openai.com/v1", "sk-xxx"))?;
//! let params = ChatParams::new("gpt-4o")
//!     .with_tools(vec![Tool::function(
//!         "get_weather",
//!         "查询城市天气",
//!         serde_json::json!({
//!             "type": "object",
//!             "properties": { "city": { "type": "string" } },
//!             "required": ["city"]
//!         }),
//!     )])
//!     .with_tool_choice(ToolChoice::auto());
//!
//! let completion = client.chat(vec![ChatMessage::user("北京天气怎么样？")], &params).await?;
//! for call in completion.tool_calls() {
//!     println!("调用 {} 参数 {}", call.function.name, call.function.arguments);
//! }
//! # Ok(())
//! # }
//! ```

pub mod client;
pub mod config;
pub mod error;
pub mod message;
pub mod metrics;
pub mod params;
pub mod response;
pub mod stream;

mod sse;

pub use client::{LlmClient, RequestOptions};
pub use config::{AuthStyle, ClientConfig, DebugHook};
pub use error::{LlmError, Result};
pub use message::{
    ChatMessage, ContentPart, FunctionCall, ImageUrl, MessageContent, Role, ToolCall,
};
pub use metrics::{
    calc_itl_stats, calc_tokens_per_second, percentile, tokens_per_second, ItlStats, StreamTiming,
};
pub use params::{
    ChatParams, EmbeddingInput, EmbeddingParams, FunctionDefinition, ResponseFormat, StreamOptions,
    Tool, ToolChoice, ToolChoiceMode,
};
pub use response::{
    ChatCompletion, Choice, CompletionTokensDetails, Embedding, EmbeddingResponse, ModelInfo,
    PromptTokensDetails, ResponseMessage, Timings, Usage,
};
pub use stream::{
    ChatStream, ChoiceAccumulator, FinishInfo, FunctionCallDelta, StreamAccumulator, StreamEvent,
    ToolCallDelta,
};
