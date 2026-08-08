use crate::llm_tool::{ChatMessage, ChatParams, ClientConfig, LlmClient, StreamEvent, FinishInfo};
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
        reasoning_content: None,
        finished: true,
        error: Some(error),
        duration_ms: start.elapsed().as_millis(),
        completion_tokens_per_second: None,
        prompt_tokens_per_second: None,
    });
}

/// 发送正文流式 chunk 到前端
fn emit_chunk(
    app: &tauri::AppHandle,
    window_id: usize,
    start: std::time::Instant,
    chunk: &str,
) {
    let _ = app.emit("stream_chunk", StreamChunkEvent {
        window_id,
        content: chunk.to_string(),
        reasoning_content: None,
        finished: false,
        error: None,
        duration_ms: start.elapsed().as_millis(),
        completion_tokens_per_second: None,
        prompt_tokens_per_second: None,
    });
}

/// 发送思考内容流式 chunk 到前端
fn emit_reasoning_chunk(
    app: &tauri::AppHandle,
    window_id: usize,
    start: std::time::Instant,
    chunk: &str,
) {
    let _ = app.emit("stream_chunk", StreamChunkEvent {
        window_id,
        content: String::new(),
        reasoning_content: Some(chunk.to_string()),
        finished: false,
        error: None,
        duration_ms: start.elapsed().as_millis(),
        completion_tokens_per_second: None,
        prompt_tokens_per_second: None,
    });
}

/// 发送成功完成事件到前端
fn emit_finished(
    app: &tauri::AppHandle,
    window_id: usize,
    start: std::time::Instant,
    completion_tps: Option<f64>,
    prompt_tps: Option<f64>,
) {
    let _ = app.emit("stream_chunk", StreamChunkEvent {
        window_id,
        content: String::new(),
        reasoning_content: None,
        finished: true,
        error: None,
        duration_ms: start.elapsed().as_millis(),
        completion_tokens_per_second: completion_tps,
        prompt_tokens_per_second: prompt_tps,
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
                reasoning_content: None,
                duration_ms: start.elapsed().as_millis(),
                completion_tokens_per_second: None,
                prompt_tokens_per_second: None,
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
                reasoning_content: None,
                duration_ms: start.elapsed().as_millis(),
                completion_tokens_per_second: None,
                prompt_tokens_per_second: None,
                error: Some(error_msg),
            };
        }
    };

    // 6. 逐事件处理流式响应
    let mut accumulated_content = String::new();
    let mut accumulated_reasoning = String::new();
    let mut last_error: Option<String> = None;
    let mut finish_info: Option<FinishInfo> = None;
    let mut got_finish = false; // 是否已收到 finish_reason: stop 事件

    while let Some(event_result) = rx.recv().await {
        match event_result {
            Ok(event) => match event {
                StreamEvent::ReasoningDelta(s) => {
                    // 推理模型的思考内容，推送给前端展示
                    if !s.is_empty() {
                        accumulated_reasoning.push_str(&s);
                        emit_reasoning_chunk(&app, window_id, start, &s);
                    }
                }
                StreamEvent::ContentDelta(s) => {
                    // 正文回复内容，推送给前端
                    if !s.is_empty() {
                        accumulated_content.push_str(&s);
                        emit_chunk(&app, window_id, start, &s);
                    }
                }
                StreamEvent::Finish(info) => {
                    // 某些 API 的 finish 事件不带 usage，usage 在后续 chunk 中。
                    // 因此不立即 break，继续消费直到 [DONE]，用最后收到的 usage 覆盖。
                    if info.usage.is_some() {
                        // 本次 Finish 已携带 usage，直接使用
                        finish_info = Some(info);
                    } else if !got_finish {
                        // 首次收到 finish（通常不带 usage），记录但继续等待
                        finish_info = Some(info);
                        got_finish = true;
                    } else {
                        // 后续又收到 Finish（携带 usage），覆盖
                        finish_info = Some(info);
                    }
                }
            },
            Err(e) => {
                last_error = Some(format!("流式事件解析失败: {e}"));
                break;
            }
        }
    }

    // 7. 计算 token 速度（优先从 FinishInfo.usage 反推，无 usage 则为 None）
    let duration_ms = start.elapsed().as_millis();
    let (completion_tps, prompt_tps) = finish_info
        .as_ref()
        .and_then(|info| info.usage.as_ref())
        .map(|usage| {
            let completion_tps = if usage.completion_tokens > 0 && duration_ms > 0 {
                Some(usage.completion_tokens as f64 / (duration_ms as f64 / 1000.0))
            } else {
                None
            };
            let prompt_tps = if usage.prompt_tokens > 0 && duration_ms > 0 {
                Some(usage.prompt_tokens as f64 / (duration_ms as f64 / 1000.0))
            } else {
                None
            };
            (completion_tps, prompt_tps)
        })
        .unwrap_or((None, None));

    // 调试日志：打印 token 速度计算结果
    eprintln!("[llmperf] window={window_id} duration_ms={duration_ms} completion_tps={:?} prompt_tps={:?}", completion_tps, prompt_tps);

    // 8. 发送完成事件（携带 token 速度）
    emit_finished(&app, window_id, start, completion_tps, prompt_tps);

    // 9. 返回聚合结果
    SingleResult {
        window_id,
        assistant_content: accumulated_content,
        duration_ms,
        completion_tokens_per_second: completion_tps,
        prompt_tokens_per_second: prompt_tps,
        error: last_error,
        reasoning_content: Some(accumulated_reasoning),
    }
}
