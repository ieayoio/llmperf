use crate::llm_tool::{ChatMessage, ChatParams, ClientConfig, LlmClient, StreamEvent, FinishInfo};
use crate::types::{LLMRequestConfig, SingleResult, StreamChunkEvent};
use tauri::Emitter;

/// 根据 usage 与两个时间点计算 token/s。
///
/// 参数语义：
/// - `total_duration_ms`: 整个请求的总耗时（毫秒），包含网络握手、prefill、补全阶段
/// - `first_content_at_ms`: 首个正文 ContentDelta 到达的时间点（毫秒），
///   None 表示无法区分 prefill 阶段（例如流立即断开或模型在 prefill 后未返回正文）
///
/// 返回值：
/// - `completion_tokens_per_second`: 优先用 `total - prefill` 算补全速度；退化用 total
/// - `prompt_tokens_per_second`: 必须能区分 prefill 时长才有意义，否则 None
///   （否则数值会接近 completion_tps，参考价值为零 —— 这就是 B1 修的根源）
pub(crate) fn calc_tokens_per_second(
    usage: Option<&crate::llm_tool::Usage>,
    total_duration_ms: u128,
    first_content_at_ms: Option<u128>,
) -> (Option<f64>, Option<f64>) {
    let Some(usage) = usage else {
        return (None, None);
    };
    // 补全阶段耗时 = 总耗时 - prefill 耗时；无 prefill 数据时退回总耗时
    let completion_duration_ms = match first_content_at_ms {
        Some(t) if total_duration_ms > t => total_duration_ms - t,
        _ => total_duration_ms,
    };
    let completion_tps = if usage.completion_tokens > 0 && completion_duration_ms > 0 {
        Some(usage.completion_tokens as f64 / (completion_duration_ms as f64 / 1000.0))
    } else {
        None
    };
    // prompt 阶段：必须有合法的 prefill 时间点（> 0 且 ≤ 总耗时）才能算，否则置 None
    let prompt_tps = match first_content_at_ms {
        Some(prefill_ms)
            if prefill_ms > 0 && prefill_ms <= total_duration_ms && usage.prompt_tokens > 0 =>
        {
            Some(usage.prompt_tokens as f64 / (prefill_ms as f64 / 1000.0))
        }
        _ => None,
    };
    (completion_tps, prompt_tps)
}

/// 发送错误事件到前端
fn emit_error(
    app: &tauri::AppHandle,
    window_id: usize,
    start: std::time::Instant,
    error: String,
) {
    let _ = app.emit("stream_chunk", StreamChunkEvent {
        target_window_id: window_id,
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
        target_window_id: window_id,
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
        target_window_id: window_id,
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
        target_window_id: window_id,
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
///
/// 透传 `reasoning_content` 字段：部分推理模型（如 deepseek-reasoner）在多轮对话时
/// 要求 assistant 历史消息携带上一轮的推理链，否则会校验失败或答复降级。
fn convert_messages(messages: &[crate::types::ChatMessage]) -> Vec<ChatMessage> {
    messages
        .iter()
        .map(|m| {
            let base = match m.role.to_lowercase().as_str() {
                "system" => ChatMessage::system(&m.content),
                "assistant" => ChatMessage::assistant(&m.content),
                _ => ChatMessage::user(&m.content), // 默认 user
            };
            // assistant 消息透传 reasoning_content；其他角色忽略
            if matches!(base.role, crate::llm_tool::Role::Assistant) {
                if let Some(r) = m.reasoning_content.as_deref() {
                    if !r.is_empty() {
                        return ChatMessage::assistant_with_reasoning(&m.content, r);
                    }
                }
            }
            base
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
    // 首个正文 chunk 到达的时间（毫秒），用于估算 prefill 阶段的耗时
    let mut first_content_at_ms: Option<u128> = None;

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
                        // 首个正文到达时记录时间点，作为 prefill 阶段结束标志
                        if first_content_at_ms.is_none() {
                            first_content_at_ms = Some(start.elapsed().as_millis());
                        }
                        accumulated_content.push_str(&s);
                        emit_chunk(&app, window_id, start, &s);
                    }
                }
                StreamEvent::Finish(info) => {
                    // 持续接收直到流结束（[DONE] 或断开），保留最后一次携带 usage 的 Finish 信息
                    if info.usage.is_some() || finish_info.is_none() {
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

    // 7. 计算 token 速度
    //    - completion_tps: 优先用 prefill 后到结束的耗时，没有则用总耗时
    //    - prompt_tps: 仅在能区分 prefill 阶段时计算（prefill 耗时 ≈ 首字延迟），
    //      否则为 None —— 用总耗时反推会得到与 completion_tps 几乎一样的数值，没有参考意义
    let total_duration_ms = start.elapsed().as_millis();
    let (completion_tps, prompt_tps) = calc_tokens_per_second(
        finish_info.as_ref().and_then(|i| i.usage.as_ref()),
        total_duration_ms,
        first_content_at_ms,
    );

    // 调试日志：打印 token 速度计算结果
    eprintln!(
        "[llmperf] window={window_id} total_ms={} completion_tps={:?} prompt_tps={:?}",
        total_duration_ms, completion_tps, prompt_tps
    );

    // 8. 发送完成事件（携带 token 速度）
    emit_finished(&app, window_id, start, completion_tps, prompt_tps);

    // 9. 返回聚合结果
    SingleResult {
        window_id,
        assistant_content: accumulated_content,
        duration_ms: total_duration_ms,
        completion_tokens_per_second: completion_tps,
        prompt_tokens_per_second: prompt_tps,
        error: last_error,
        reasoning_content: Some(accumulated_reasoning),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_tool::Usage;

    fn usage(prompt: u64, completion: u64) -> Usage {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
        }
    }

    /// B1 验证：能区分 prefill 时长时，prompt_tps 用 prefill 时间反推，
    /// 且与 completion_tps 数值差异应当显著（之前用总耗时算会几乎相等）。
    #[test]
    fn test_calc_tokens_per_second_with_prefill() {
        // 假设 prefill 300ms（30 tokens），补全 1500ms（150 tokens），
        // 总耗时 1800ms。
        let u = usage(30, 150);
        let (comp, prompt) = calc_tokens_per_second(Some(&u), 1800, Some(300));
        // completion = 150 / 1.5 = 100 tok/s
        assert!((comp.unwrap() - 100.0).abs() < 0.01, "completion_tps 应对应补全阶段");
        // prompt = 30 / 0.3 = 100 tok/s，这里恰好相等但来源不同，是数学巧合；
        // 关键断言：prompt_tps 应当不等于 "150 / 1.8 = 83.33"
        let wrong = 150.0 / 1.8;
        assert!(
            (prompt.unwrap() - wrong).abs() > 1.0,
            "prompt_tps 不应再用总耗时推算（B1 修复）"
        );
    }

    /// B1 验证：无 first_content_at_ms 时，prompt_tps 应当为 None，
    /// 而 completion_tps 退回到用总耗时计算。
    #[test]
    fn test_calc_tokens_per_second_no_prefill() {
        let u = usage(30, 150);
        let (comp, prompt) = calc_tokens_per_second(Some(&u), 1800, None);
        assert!(comp.is_some(), "completion_tps 仍应可用总耗时推算");
        assert!(prompt.is_none(), "无 prefill 数据时 prompt_tps 必须为 None");
    }

    /// B2 关联测试：无 usage 时两个值都为 None。
    #[test]
    fn test_calc_tokens_per_second_no_usage() {
        let (comp, prompt) = calc_tokens_per_second(None, 1000, Some(200));
        assert!(comp.is_none());
        assert!(prompt.is_none());
    }

    /// 边界：first_content_at_ms 大于 total_duration_ms（异常），退回总耗时。
    #[test]
    fn test_calc_tokens_per_second_prefill_greater_than_total() {
        let u = usage(10, 100);
        let (comp, prompt) = calc_tokens_per_second(Some(&u), 500, Some(800));
        // completion 用 500ms：100/0.5 = 200
        assert!((comp.unwrap() - 200.0).abs() < 0.01);
        assert!(prompt.is_none(), "prefill > total 是异常，不该输出 prompt_tps");
    }

    /// 边界：0 token / 0 ms 不应除零。
    #[test]
    fn test_calc_tokens_per_second_zero_tokens() {
        let u = usage(0, 0);
        let (comp, prompt) = calc_tokens_per_second(Some(&u), 1000, Some(200));
        assert!(comp.is_none());
        assert!(prompt.is_none());
    }
}
