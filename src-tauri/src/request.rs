use crate::llm_tool::{ChatMessage, ChatParams, ClientConfig, LlmClient, StreamEvent, FinishInfo, Timings};
use crate::types::{LLMRequestConfig, SingleResult, StreamChunkEvent};
use tauri::Emitter;
use tokio_util::sync::CancellationToken;

/// 根据 timings / usage 计算 token/s。
///
/// ## 数据源优先级
/// 1. **timings**（vLLM 等 API 在最后一个流式 chunk 给出，权威）
///    - `prompt_tps = timings.prompt_per_second`（基于 `prompt_n`，已扣除 cache 命中）
///    - `completion_tps = timings.predicted_per_second`（基于 `predicted_n` + `predicted_ms`）
/// 2. **退化路径**（timings 缺失）：用 `usage` + 时间戳反推
///    - `completion_tps = usage.completion_tokens / (total - prefill)`
///    - `prompt_tps = usage.prompt_tokens / prefill`，**仅在能区分 prefill 时**返回，
///      否则 None —— 否则 `prompt_tokens / total` 会得到与 completion_tps 几乎一样的数值，
///      完全没有参考意义（B1 修复的根源）
///
/// ## 为什么 prompt_tps 不能简单用 usage.prompt_tokens
/// `usage.prompt_tokens` 通常包含 `prompt_tokens_details.cached_tokens`（cache 命中不计费），
/// 而 `timings.prompt_n` 才是真实参与计算的 token 数。两者直接相除会得到 4-5 倍的偏高估计。
///
/// ## 参数语义
/// - `timings`: API 在最后一个 chunk 给出的真实速度字段（vLLM 兼容）
/// - `usage`: API 给出的 token 使用量（兜底数据源）
/// - `total_duration_ms`: 整个请求的总耗时，包含网络握手、prefill、补全阶段
/// - `first_content_at_ms`: 首个正文 ContentDelta 到达的时间点，
///   None 表示无法区分 prefill 阶段
///
/// ## 返回
/// - `completion_tokens_per_second`: 补全阶段 token/s
/// - `prompt_tokens_per_second`: Prompt 阶段 token/s（无法可靠计算时为 None）
pub(crate) fn calc_tokens_per_second(
    timings: Option<&Timings>,
    usage: Option<&crate::llm_tool::Usage>,
    total_duration_ms: u128,
    first_content_at_ms: Option<u128>,
) -> (Option<f64>, Option<f64>) {
    // === 优先：timings（vLLM 风格） ===
    if let Some(t) = timings {
        let completion_tps = t.completion_tokens_per_second();
        let prompt_tps = t.prompt_tokens_per_second();
        // timings 中任一值非 0 即视为有效；两者都缺失才退化
        if completion_tps.is_some() || prompt_tps.is_some() {
            return (completion_tps, prompt_tps);
        }
    }

    // === 退化路径：usage + 时间戳反推 ===
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
    // 注意：使用 usage.prompt_tokens（包含 cache）会有偏差，但比完全无数据好。
    // 真实场景下应优先走 timings 路径避开此问题。
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

/// 发送"用户取消"完成事件：复用 finished 通道，前端走现有 error 分支
fn emit_canceled(
    app: &tauri::AppHandle,
    window_id: usize,
    start: std::time::Instant,
) {
    let _ = app.emit("stream_chunk", StreamChunkEvent {
        target_window_id: window_id,
        content: String::new(),
        reasoning_content: None,
        finished: true,
        // 用统一的"用户已取消"文案，便于前端展示
        error: Some("用户已取消".to_string()),
        duration_ms: start.elapsed().as_millis(),
        completion_tokens_per_second: None,
        prompt_tokens_per_second: None,
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
///
/// ## 取消语义
/// 在事件循环中同时监听 `rx.recv()` 与 `cancel_token.cancelled()`：
/// - 若收到取消信号，立刻 break 循环并发出 `finished + 用户已取消` 事件
/// - 已累积的正文/思考内容会保留在 `accumulated_*` 字段，前端可以读到部分输出
pub async fn execute_stream_request(
    app: tauri::AppHandle,
    window_id: usize,
    config: LLMRequestConfig,
    cancel_token: CancellationToken,
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

    // 6. 逐事件处理流式响应（同时监听取消信号）
    let mut accumulated_content = String::new();
    let mut accumulated_reasoning = String::new();
    let mut last_error: Option<String> = None;
    let mut finish_info: Option<FinishInfo> = None;
    // 首个正文 chunk 到达的时间（毫秒），用于估算 prefill 阶段的耗时
    let mut first_content_at_ms: Option<u128> = None;
    // 是否被用户主动取消
    let mut canceled = false;

    loop {
        tokio::select! {
            // 偏置：先检查取消信号，避免 cancel 后仍把 buffer 中的残余 chunk 推给前端
            biased;
            _ = cancel_token.cancelled() => {
                canceled = true;
                break;
            }
            event_result = rx.recv() => {
                match event_result {
                    Some(Ok(event)) => match event {
                        StreamEvent::ReasoningDelta(s) => {
                            if !s.is_empty() {
                                accumulated_reasoning.push_str(&s);
                                emit_reasoning_chunk(&app, window_id, start, &s);
                            }
                        }
                        StreamEvent::ContentDelta(s) => {
                            if !s.is_empty() {
                                if first_content_at_ms.is_none() {
                                    first_content_at_ms = Some(start.elapsed().as_millis());
                                }
                                accumulated_content.push_str(&s);
                                emit_chunk(&app, window_id, start, &s);
                            }
                        }
                        StreamEvent::Finish(info) => {
                            if info.usage.is_some() || finish_info.is_none() {
                                finish_info = Some(info);
                            }
                        }
                    },
                    Some(Err(e)) => {
                        last_error = Some(format!("流式事件解析失败: {e}"));
                        break;
                    }
                    None => break, // 流自然结束
                }
            }
        }
    }

    // 7. 计算 token 速度
    //    优先使用 timings（vLLM 等 API 在流末尾提供），其次退化到 usage + 时间戳反推
    let total_duration_ms = start.elapsed().as_millis();
    let (completion_tps, prompt_tps) = calc_tokens_per_second(
        finish_info.as_ref().and_then(|i| i.timings.as_ref()),
        finish_info.as_ref().and_then(|i| i.usage.as_ref()),
        total_duration_ms,
        first_content_at_ms,
    );

    // 调试日志：打印 token 速度计算结果
    eprintln!(
        "[llmperf] window={window_id} total_ms={} completion_tps={:?} prompt_tps={:?}",
        total_duration_ms, completion_tps, prompt_tps
    );

    // 8. 根据是否被取消走不同的完成事件
    if canceled {
        // 取消：发送统一的"用户已取消"完成事件，error 字段会被前端当成错误状态展示
        emit_canceled(&app, window_id, start);
        return SingleResult {
            window_id,
            assistant_content: accumulated_content,
            duration_ms: total_duration_ms,
            completion_tokens_per_second: None,
            prompt_tokens_per_second: None,
            // 返回值的 error 也要带上"用户已取消"，便于调用方区分
            error: Some("用户已取消".to_string()),
            reasoning_content: Some(accumulated_reasoning),
        };
    }

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
        let (comp, prompt) = calc_tokens_per_second(None, Some(&u), 1800, Some(300));
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
        let (comp, prompt) = calc_tokens_per_second(None, Some(&u), 1800, None);
        assert!(comp.is_some(), "completion_tps 仍应可用总耗时推算");
        assert!(prompt.is_none(), "无 prefill 数据时 prompt_tps 必须为 None");
    }

    /// B2 关联测试：无 usage 时两个值都为 None。
    #[test]
    fn test_calc_tokens_per_second_no_usage() {
        let (comp, prompt) = calc_tokens_per_second(None, None, 1000, Some(200));
        assert!(comp.is_none());
        assert!(prompt.is_none());
    }

    /// 边界：first_content_at_ms 大于 total_duration_ms（异常），退回总耗时。
    #[test]
    fn test_calc_tokens_per_second_prefill_greater_than_total() {
        let u = usage(10, 100);
        let (comp, prompt) = calc_tokens_per_second(None, Some(&u), 500, Some(800));
        // completion 用 500ms：100/0.5 = 200
        assert!((comp.unwrap() - 200.0).abs() < 0.01);
        assert!(prompt.is_none(), "prefill > total 是异常，不该输出 prompt_tps");
    }

    /// 边界：0 token / 0 ms 不应除零。
    #[test]
    fn test_calc_tokens_per_second_zero_tokens() {
        let u = usage(0, 0);
        let (comp, prompt) = calc_tokens_per_second(None, Some(&u), 1000, Some(200));
        assert!(comp.is_none());
        assert!(prompt.is_none());
    }

    /// 真实场景：有 timings 时应优先使用 timings 中的速度（vLLM 风格 API），
    /// 即使 usage + 时间戳反推会得出不同结果。模拟 API 返回：
    ///   timings.prompt_per_second = 40.94, predicted_per_second = 38.63
    ///   usage.prompt_tokens = 18（含 14 cache），completion_tokens = 42
    ///   first_content_at_ms = 97（prefill）
    ///   total_duration_ms = 1500
    /// 退化路径会算：prompt_tps = 18/0.0977 = 184（偏高 4.5x）
    /// timings 路径应当返回真实速度 40.94 / 38.63
    #[test]
    fn test_calc_tokens_per_second_prefer_timings() {
        let timings = Timings {
            prompt_n: 4,
            prompt_ms: 97.702,
            prompt_per_token_ms: 24.4,
            prompt_per_second: 40.94,
            predicted_n: 42,
            predicted_ms: 1087.156,
            predicted_per_token_ms: 25.88,
            predicted_per_second: 38.63,
        };
        let usage = Usage {
            prompt_tokens: 18,
            completion_tokens: 42,
            total_tokens: 60,
        };
        let (comp, prompt) = calc_tokens_per_second(
            Some(&timings),
            Some(&usage),
            1500,
            Some(97),
        );
        // 关键断言：使用 timings 真实值，而非退化路径的 184
        assert!((comp.unwrap() - 38.63).abs() < 0.01, "completion_tps 必须取 timings.predicted_per_second");
        assert!((prompt.unwrap() - 40.94).abs() < 0.01, "prompt_tps 必须取 timings.prompt_per_second");
    }

    /// 边界：timings 字段全为 0（API 返回空 timings）时应退化到 usage 反推
    #[test]
    fn test_calc_tokens_per_second_timings_all_zero_falls_back() {
        let timings = Timings::default(); // 全 0
        let u = usage(30, 150);
        let (comp, prompt) = calc_tokens_per_second(Some(&timings), Some(&u), 1800, Some(300));
        // 退化路径：completion = 150/1.5 = 100，prompt = 30/0.3 = 100
        assert!((comp.unwrap() - 100.0).abs() < 0.01);
        assert!((prompt.unwrap() - 100.0).abs() < 0.01);
    }
}
