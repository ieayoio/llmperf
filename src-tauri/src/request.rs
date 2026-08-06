use crate::llm_tool::{ChatMessage, ChatParams, ClientConfig, LlmClient, StreamEvent};
use crate::types::{LLMRequestConfig, SingleResult, StreamChunkEvent};
use tauri::Emitter;

/// 发送错误事件到前端
fn emit_error(
    app: &tauri::AppHandle,
    window_id: usize,
    start: std::time::Instant,
    error: String,
) {
    let _ = app.emit("stream_chunk", StreamChunkEvent {
        window_id,
        content: String::new(),
        finished: true,
        error: Some(error),
        duration_ms: start.elapsed().as_millis(),
    });
}

/// 发送流式 chunk 到前端
fn emit_chunk(
    app: &tauri::AppHandle,
    window_id: usize,
    start: std::time::Instant,
    chunk: &str,
) {
    let _ = app.emit("stream_chunk", StreamChunkEvent {
        window_id,
        content: chunk.to_string(),
        finished: false,
        error: None,
        duration_ms: start.elapsed().as_millis(),
    });
}

/// 发送成功完成事件到前端
fn emit_finished(
    app: &tauri::AppHandle,
    window_id: usize,
    start: std::time::Instant,
) {
    let _ = app.emit("stream_chunk", StreamChunkEvent {
        window_id,
        content: String::new(),
        finished: true,
        error: None,
        duration_ms: start.elapsed().as_millis(),
    });
}

/// 将前端传入的 `ChatMessage` (简单结构) 转换为 `LlmClient` 的 `ChatMessage` (带角色枚举)
fn convert_messages(messages: &[crate::types::ChatMessage]) -> Vec<ChatMessage> {
    messages
        .iter()
        .map(|m| {
            let role = match m.role.to_lowercase().as_str() {
                "system" => ChatMessage::system(&m.content),
                "assistant" => ChatMessage::assistant(&m.content),
                _ => ChatMessage::user(&m.content), // 默认 user
            };
            role
        })
        .collect()
}

/// 执行单次流式请求，返回最终结果
///
/// 该函数通过 `LlmClient` 发送流式请求，逐块接收 SSE 响应并推送给前端：
/// 1. 构造 `LlmClient` 所需的 `ChatMessage` 和 `ChatParams`
/// 2. 调用 `client.chat_stream()` 获取事件流
/// 3. 逐事件转换为 `StreamChunkEvent` 推送给前端
/// 4. 返回聚合的完整内容
pub async fn execute_stream_request(
    app: tauri::AppHandle,
    window_id: usize,
    config: LLMRequestConfig,
) -> SingleResult {
    let start = std::time::Instant::now();

    // 1. 构建 LlmClient 客户端配置
    let client_config = ClientConfig {
        base_url: config.base_url.trim().to_string(),
        api_key: config.api_key.trim().to_string(),
        timeout_secs: 120,
    };

    // 2. 创建客户端
    let client = match LlmClient::new(client_config) {
        Ok(c) => c,
        Err(e) => {
            let error_msg = format!("客户端初始化失败: {e}");
            emit_error(&app, window_id, start, error_msg.clone());
            return SingleResult {
                window_id,
                assistant_content: String::new(),
                duration_ms: start.elapsed().as_millis(),
                error: Some(error_msg),
            };
        }
    };

    // 3. 转换消息格式（前端简单结构 → LlmClient 带枚举的结构）
    let messages = convert_messages(&config.messages);

    // 4. 构造模型参数
    let params = ChatParams {
        model: config.model.trim().to_string(),
        ..Default::default()
    };

    // 5. 发送流式请求，获取事件接收端
    let mut rx = match client.chat_stream(messages, &params).await {
        Ok(rx) => rx,
        Err(e) => {
            let error_msg = format!("流式请求发送失败: {e}");
            emit_error(&app, window_id, start, error_msg.clone());
            return SingleResult {
                window_id,
                assistant_content: String::new(),
                duration_ms: start.elapsed().as_millis(),
                error: Some(error_msg),
            };
        }
    };

    // 6. 逐事件处理流式响应
    let mut accumulated = String::new();
    let mut last_error: Option<String> = None;

    while let Some(event_result) = rx.recv().await {
        match event_result {
            Ok(event) => match event {
                StreamEvent::ReasoningDelta(s) => {
                    // 推理模型的思考内容（可选，不推送给前端）
                    accumulated.push_str(&s);
                }
                StreamEvent::ContentDelta(s) => {
                    // 正文回复内容，推送给前端
                    if !s.is_empty() {
                        accumulated.push_str(&s);
                        emit_chunk(&app, window_id, start, &s);
                    }
                }
                StreamEvent::Finish(_info) => {
                    // 流结束，不再处理后续事件
                    break;
                }
            },
            Err(e) => {
                last_error = Some(format!("流式事件解析失败: {e}"));
                break;
            }
        }
    }

    // 7. 发送完成事件
    emit_finished(&app, window_id, start);

    // 8. 返回聚合结果
    SingleResult {
        window_id,
        assistant_content: accumulated,
        duration_ms: start.elapsed().as_millis(),
        error: last_error,
    }
}
