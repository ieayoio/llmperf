use serde::{Deserialize, Serialize};

/// 单个窗口的最终请求结果
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SingleResult {
    pub window_id: usize,
    pub assistant_content: String,
    pub duration_ms: u128,
    pub error: Option<String>,
}

/// 流式 chunk 事件
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StreamChunkEvent {
    pub window_id: usize,
    pub content: String,
    pub finished: bool,
    pub error: Option<String>,
    pub duration_ms: u128,
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
