use serde::{Deserialize, Serialize};

/// 单个窗口的最终请求结果
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SingleResult {
    pub window_id: usize,
    pub assistant_content: String,
    /// 思考/推理内容（推理模型会返回）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    pub duration_ms: u128,
    /// 补全阶段 token 速度（每秒 token 数），无 timings 时由 usage / duration_ms 反推
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens_per_second: Option<f64>,
    /// Prompt 阶段 token 速度（每秒 token 数），无 timings 时由 usage / duration_ms 反推
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_per_second: Option<f64>,
    pub error: Option<String>,
}

/// 流式 chunk 事件
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StreamChunkEvent {
    pub window_id: usize,
    /// 正文回复增量（非空时表示此事件为正文内容）
    pub content: String,
    /// 思考/推理内容增量（非空时表示此事件为思考过程）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    pub finished: bool,
    pub error: Option<String>,
    pub duration_ms: u128,
    /// 补全阶段 token 速度（每秒 token 数，无数据时为 null）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens_per_second: Option<f64>,
    /// Prompt 阶段 token 速度（每秒 token 数，无数据时为 null）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_per_second: Option<f64>,
}

/// 前端传入的配置
#[derive(Deserialize, Debug, Clone)]
pub struct LLMRequestConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 完整的对话消息历史 (user/assistant 交替)
    pub messages: Vec<ChatMessage>,
    /// 窗口标识，用于区分不同窗口的流式事件
    pub window_id: usize,
}

/// 单条聊天消息
#[derive(Deserialize, Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}
