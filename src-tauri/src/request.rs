use crate::llm_tool::{ChatMessage, ChatParams, ClientConfig, LlmClient, StreamEvent, FinishInfo, Timings};
use crate::types::{LLMRequestConfig, SingleResult, StreamChunkEvent};
use std::collections::HashMap;
use std::sync::OnceLock;
use tauri::Emitter;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// 用户主动取消时的统一文案
///
/// 目前同时作为前后端约定的判别值（前端据 `error` 内容识别取消态），
/// 修改时必须同步调整 `src/App.tsx`。
const CANCELED_MESSAGE: &str = "用户已取消";

/// 默认请求超时（秒）
///
/// reqwest 的 `timeout` 覆盖整个请求生命周期（含排队与逐 token 生成），
/// 推理模型的长输出很容易超过 2 分钟，因此默认放宽到 10 分钟；
/// 前端可通过 `LLMRequestConfig.timeout_secs` 覆盖。
const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// 判定 tps 所需的最小阶段时长（毫秒）
///
/// 阶段时长过短时（例如所有 token 在一个 chunk 里一次性到达），
/// `tokens / duration` 会被放大成毫无参考价值的天文数字，直接视为无数据。
const MIN_PHASE_DURATION_MS: u128 = 10;

// ============================ 客户端缓存 ============================

/// 客户端缓存键
///
/// `reqwest::Client` 的超时在构建时固定，因此超时值变化也必须重建客户端。
#[derive(Clone, PartialEq, Eq, Hash)]
struct ClientKey {
    base_url: String,
    api_key: String,
    timeout_secs: u64,
}

/// 缓存条目上限：超出后整体清空，避免长时间运行下无界增长
const CLIENT_CACHE_LIMIT: usize = 16;

/// 全局客户端缓存
///
/// 压测场景必须复用连接池：每次新建 `reqwest::Client` 都会重新做 DNS/TCP/TLS 握手，
/// 握手耗时会被计入 TTFT 与总耗时，从而污染 token/s 统计；
/// 并发时还会造成大量短连接，瓶颈变成客户端自己而不是被测服务。
static CLIENT_CACHE: OnceLock<Mutex<HashMap<ClientKey, LlmClient>>> = OnceLock::new();

fn client_cache() -> &'static Mutex<HashMap<ClientKey, LlmClient>> {
    CLIENT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 获取可复用的客户端：命中缓存直接返回克隆，否则创建并写入缓存
async fn get_client(config: ClientConfig) -> crate::llm_tool::Result<LlmClient> {
    let key = ClientKey {
        base_url: config.base_url.clone(),
        api_key: config.api_key.clone(),
        timeout_secs: config.timeout_secs,
    };

    let mut cache = client_cache().lock().await;
    if let Some(client) = cache.get(&key) {
        return Ok(client.clone());
    }

    // 创建过程是同步的（不发起网络请求），持锁时间极短
    let client = LlmClient::new(config)?;
    // 超出上限时整体清空：桌面工具场景缓存条目极少，无需精细淘汰策略
    if cache.len() >= CLIENT_CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key, client.clone());
    Ok(client)
}

/// 通用 tps 计算：tokens / 秒；零 token 或窗口过短时返回 None
fn tokens_per_second(tokens: u64, duration_ms: u128) -> Option<f64> {
    if tokens == 0 || duration_ms < MIN_PHASE_DURATION_MS {
        return None;
    }
    let tps = tokens as f64 / (duration_ms as f64 / 1000.0);
    (tps > 0.0 && tps.is_finite()).then_some(tps)
}

/// 根据 timings / usage 计算 token/s。
///
/// ## 数据源优先级
/// 1. **timings**（vLLM 等 API 在最后一个流式 chunk 给出，权威）
///    - `prompt_tps = timings.prompt_per_second`（基于 `prompt_n`，已扣除 cache 命中）
///    - `completion_tps = timings.predicted_per_second`（基于 `predicted_n` + `predicted_ms`）
/// 2. **退化路径**（timings 缺失）：用 `usage` + 时间戳反推
///    - `completion_tps = usage.completion_tokens / (total - prefill)`
///    - `prompt_tps = usage.uncached_prompt_tokens() / prefill`，**仅在能区分 prefill 时**返回，
///      否则 None —— 否则 `prompt_tokens / total` 会得到与 completion_tps 几乎一样的数值，
///      完全没有参考意义（B1 修复的根源）
///
/// ## 按字段独立回退
/// timings 中 prompt / completion 两项互相独立：例如 prefix cache 全命中时
/// `prompt_n = 0`（prompt 项无效），但 completion 项仍然可用；此时只对缺失项退化。
///
/// ## 为什么 prompt_tps 不能简单用 usage.prompt_tokens
/// `usage.prompt_tokens` 通常包含 `prompt_tokens_details.cached_tokens`（cache 命中不计费），
/// 而 `timings.prompt_n` 才是真实参与计算的 token 数。两者直接相除会得到 4-5 倍的偏高估计。
///
/// ## 参数语义
/// - `timings`: API 在最后一个 chunk 给出的真实速度字段（vLLM 兼容）
/// - `usage`: API 给出的 token 使用量（兜底数据源）
/// - `total_duration_ms`: 整个请求的总耗时，包含网络握手、prefill、补全阶段
/// - `first_token_at_ms`: 首个增量（**思考或正文**均可）到达的时间点，
///   None 表示无法区分 prefill 阶段
///
/// ## 为什么锚点必须是"首个增量"而不是"首个正文"
/// 推理模型会先输出很长的思考内容再输出正文，而 `usage.completion_tokens`
/// 已包含思考 token。若用首个正文时刻切分，会把整段思考生成时间算进 prefill，
/// 导致 `completion_tps` 高估（实测可达 2~5 倍）、`prompt_tps` 低估。
///
/// ## 返回
/// - `completion_tokens_per_second`: 补全阶段 token/s
/// - `prompt_tokens_per_second`: Prompt 阶段 token/s（无法可靠计算时为 None）
pub(crate) fn calc_tokens_per_second(
    timings: Option<&Timings>,
    usage: Option<&crate::llm_tool::Usage>,
    total_duration_ms: u128,
    first_token_at_ms: Option<u128>,
) -> (Option<f64>, Option<f64>) {
    // === 1. timings 优先（两项独立取舍：缺一项不影响另一项） ===
    let timings_completion = timings.and_then(|t| t.completion_tokens_per_second());
    let timings_prompt = timings.and_then(|t| t.prompt_tokens_per_second());

    let Some(usage) = usage else {
        return (timings_completion, timings_prompt);
    };

    // === 2. 阶段切分：prefill = 首 token 时刻，补全 = 剩余时长 ===
    // 首 token 时刻缺失或异常（> 总耗时）时不切分，补全阶段退回总耗时
    let prefill_ms = first_token_at_ms.filter(|t| *t > 0 && *t <= total_duration_ms);
    let completion_ms = match prefill_ms {
        Some(prefill) => total_duration_ms - prefill,
        None => total_duration_ms,
    };

    // === 3. 补全阶段退化：usage.completion_tokens / 补全时长 ===
    let completion_tps =
        timings_completion.or_else(|| tokens_per_second(usage.completion_tokens, completion_ms));

    // === 4. Prompt 阶段退化：必须能区分 prefill，且扣掉 cache 命中 token ===
    // 拿不到 cache 明细时按未命中处理（uncached_prompt_tokens 会返回 prompt_tokens）
    let prompt_tps = timings_prompt.or_else(|| match prefill_ms {
        Some(prefill) => tokens_per_second(usage.uncached_prompt_tokens(), prefill),
        None => None,
    });

    (completion_tps, prompt_tps)
}

/// 发送错误事件到前端
fn emit_error(app: &tauri::AppHandle, window_id: usize, duration_ms: u128, error: String) {
    let _ = app.emit("stream_chunk", StreamChunkEvent {
        target_window_id: window_id,
        content: String::new(),
        reasoning_content: None,
        finished: true,
        error: Some(error),
        duration_ms,
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
    duration_ms: u128,
    completion_tps: Option<f64>,
    prompt_tps: Option<f64>,
) {
    let _ = app.emit("stream_chunk", StreamChunkEvent {
        target_window_id: window_id,
        content: String::new(),
        reasoning_content: None,
        finished: true,
        error: None,
        duration_ms,
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
        error: Some(CANCELED_MESSAGE.to_string()),
        duration_ms: start.elapsed().as_millis(),
        completion_tokens_per_second: None,
        prompt_tokens_per_second: None,
    });
}

/// 构造"用户已取消"的统一返回值
///
/// 取消事件已由 [`emit_canceled`] 发出，这里保证返回值与事件语义一致，
/// 并保留已累积的部分内容。
fn canceled_result(
    window_id: usize,
    duration_ms: u128,
    assistant_content: String,
    reasoning_content: String,
) -> SingleResult {
    SingleResult {
        window_id,
        assistant_content,
        duration_ms,
        completion_tokens_per_second: None,
        prompt_tokens_per_second: None,
        error: Some(CANCELED_MESSAGE.to_string()),
        reasoning_content: Some(reasoning_content),
    }
}

/// 合并流中多次到达的结束信息
///
/// 不同实现会把 usage 与 timings 拆到不同 chunk（例如 finish_reason 一个 chunk、
/// usage+timings 另一个 chunk），直接整体覆盖会让先到的 timings 丢失，
/// 因此这里按字段合并，只填补尚未拿到的部分。
fn merge_finish_info(slot: &mut Option<FinishInfo>, incoming: FinishInfo) {
    match slot {
        None => *slot = Some(incoming),
        Some(current) => {
            if current.usage.is_none() {
                current.usage = incoming.usage;
            }
            if current.timings.is_none() {
                current.timings = incoming.timings;
            }
            if current.first_token_at.is_none() {
                current.first_token_at = incoming.first_token_at;
            }
            if current.reason.is_empty() {
                current.reason = incoming.reason;
            }
        }
    }
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
        timeout_secs: config.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
    };

    // 2. 获取客户端（按 base_url / api_key / 超时复用连接池）
    let client = match get_client(client_config).await {
        Ok(c) => c,
        Err(e) => {
            let error_msg = format!("客户端初始化失败: {e}");
            emit_error(&app, window_id, start.elapsed().as_millis(), error_msg.clone());
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
    //
    // 建连（DNS/TCP/TLS + 等待响应头）可能长时间挂起，且此阶段不会产生任何流式事件；
    // 因此必须与取消信号一起 select，否则服务端卡住时取消按钮完全失效。
    let request_result = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            // 建连阶段就收到取消：SSE 流尚未建立，没有已累积内容
            emit_canceled(&app, window_id, start);
            return canceled_result(
                window_id,
                start.elapsed().as_millis(),
                String::new(),
                String::new(),
            );
        }
        result = client.chat_stream(messages, &params) => result,
    };

    let mut rx = match request_result {
        Ok(rx) => rx,
        Err(e) => {
            let error_msg = format!("流式请求发送失败: {e}");
            emit_error(&app, window_id, start.elapsed().as_millis(), error_msg.clone());
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
    // 首个增量（思考或正文）在消费端记录的时间（毫秒）
    // 仅作为 SSE 解析层时间戳缺失时的兜底（例如流异常中断、未收到 Finish 事件）
    let mut first_token_at_ms: Option<u128> = None;
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
                                // 首个增量可能是思考内容：它同样是 prefill 结束的标志
                                if first_token_at_ms.is_none() {
                                    first_token_at_ms = Some(start.elapsed().as_millis());
                                }
                                accumulated_reasoning.push_str(&s);
                                emit_reasoning_chunk(&app, window_id, start, &s);
                            }
                        }
                        StreamEvent::ContentDelta(s) => {
                            if !s.is_empty() {
                                if first_token_at_ms.is_none() {
                                    first_token_at_ms = Some(start.elapsed().as_millis());
                                }
                                accumulated_content.push_str(&s);
                                emit_chunk(&app, window_id, start, &s);
                            }
                        }
                        // 按字段合并：usage 与 timings 可能分散在不同 chunk 中到达
                        StreamEvent::Finish(info) => merge_finish_info(&mut finish_info, info),
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
    // prefill 锚点：优先用 SSE 解析层打点的首 token 时刻（更接近字节真实到达），
    // 解析层拿不到时（例如流被截断、没有 Finish 事件）退回消费端记录的时刻
    let prefill_at_ms = finish_info
        .as_ref()
        .and_then(|info| info.first_token_at)
        .map(|at| at.saturating_duration_since(start).as_millis())
        .or(first_token_at_ms);
    let (completion_tps, prompt_tps) = calc_tokens_per_second(
        finish_info.as_ref().and_then(|i| i.timings.as_ref()),
        finish_info.as_ref().and_then(|i| i.usage.as_ref()),
        total_duration_ms,
        prefill_at_ms,
    );

    // 调试日志：打印 token 速度计算结果
    eprintln!(
        "[llmperf] window={window_id} total_ms={} prefill_ms={:?} completion_tps={:?} prompt_tps={:?}",
        total_duration_ms, prefill_at_ms, completion_tps, prompt_tps
    );

    // 8. 取消：发送统一的"用户已取消"完成事件，不算错误
    if canceled {
        emit_canceled(&app, window_id, start);
        return canceled_result(
            window_id,
            total_duration_ms,
            accumulated_content,
            accumulated_reasoning,
        );
    }

    // 9. 异常收敛：解析失败、或流未收到完成标记（既无 finish_reason 也无 [DONE]）
    //    都必须告知前端，否则半截内容会被当成完整结果展示
    let error = last_error
        .or_else(|| finish_info.is_none().then(|| "流意外结束：未收到完成标记".to_string()));
    if let Some(err) = error {
        emit_error(&app, window_id, total_duration_ms, err.clone());
        return SingleResult {
            window_id,
            assistant_content: accumulated_content,
            duration_ms: total_duration_ms,
            // 半截数据算出的速度没有参考价值，不入库也不展示
            completion_tokens_per_second: None,
            prompt_tokens_per_second: None,
            error: Some(err),
            reasoning_content: Some(accumulated_reasoning),
        };
    }

    // 10. 正常完成：发送携带 token 速度的完成事件
    emit_finished(&app, window_id, total_duration_ms, completion_tps, prompt_tps);

    // 11. 返回聚合结果
    SingleResult {
        window_id,
        assistant_content: accumulated_content,
        duration_ms: total_duration_ms,
        completion_tokens_per_second: completion_tps,
        prompt_tokens_per_second: prompt_tps,
        error: None,
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
            ..Default::default()
        }
    }

    /// 构造带 cache 命中数的 usage
    fn usage_with_cache(prompt: u64, cached: u64, completion: u64) -> Usage {
        Usage {
            prompt_tokens_details: Some(crate::llm_tool::PromptTokensDetails {
                cached_tokens: cached,
            }),
            ..usage(prompt, completion)
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

    /// B1 验证：无 first_token_at_ms 时，prompt_tps 应当为 None，
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

    /// 边界：first_token_at_ms 大于 total_duration_ms（异常），退回总耗时。
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
    ///   first_token_at_ms = 97（prefill）
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
            ..Default::default()
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

    /// timings 只有一半时：缺失的那一项应当回退到 usage 反推（而非直接变 None）
    #[test]
    fn test_calc_tokens_per_second_partial_timings_falls_back_per_field() {
        // 模拟 prefix cache 全命中：prompt 项全为 0（无效），仅有补全项
        let timings = Timings {
            predicted_n: 42,
            predicted_ms: 1000.0,
            predicted_per_second: 42.0,
            ..Default::default()
        };
        let u = usage(30, 42);
        let (comp, prompt) = calc_tokens_per_second(Some(&timings), Some(&u), 1300, Some(300));
        // 补全用 timings 真值
        assert!((comp.unwrap() - 42.0).abs() < 0.01);
        // prompt 回退到 usage：30 / 0.3 = 100
        assert!(
            (prompt.expect("prompt_tps 应回退到 usage 反推") - 100.0).abs() < 0.01,
            "timings 的 prompt 项无效时应回退"
        );
    }

    /// 退化路径必须扣除 cache 命中 token，否则 prompt_tps 会明显偏高
    #[test]
    fn test_calc_tokens_per_second_prompt_deducts_cached_tokens() {
        // prompt_tokens=100 其中 90 命中 cache，真实参与计算只有 10 个
        let u = usage_with_cache(100, 90, 50);
        let (_, prompt) = calc_tokens_per_second(None, Some(&u), 1200, Some(200));
        // 10 / 0.2 = 50，而不是 100 / 0.2 = 500
        assert!((prompt.unwrap() - 50.0).abs() < 0.01, "应扣除 cache 命中 token");
    }

    /// 思考模型的场景：completion 时间窗口必须从首个推理 token 开始算，
    /// 而不是从首个正文 token 开始（否则会把思考时间漏掉 → 高估）。
    #[test]
    fn test_calc_tokens_per_second_reasoning_anchor() {
        // 总时长 1000ms：prefill 200ms，剩余 800ms 内先生成思考再输出正文，共 100 token
        let u = usage(20, 100);
        // 正确锚点（首个思考 token 在 200ms）：100 / 0.8 = 125 tok/s
        let (comp, _) = calc_tokens_per_second(None, Some(&u), 1000, Some(200));
        assert!((comp.unwrap() - 125.0).abs() < 0.01);
        // 若误用"首个正文"（998ms）作锚点，窗口只剩 2ms，已被最小窗口过滤
        let (comp_wrong, _) = calc_tokens_per_second(None, Some(&u), 1000, Some(998));
        assert!(comp_wrong.is_none(), "过短窗口应被过滤掉，不应输出天文数字");
    }

    /// usage 与 timings 分散在不同 Finish 事件中时，必须按字段合并而不是整体覆盖
    #[test]
    fn test_merge_finish_info_keeps_timings() {
        let mut slot: Option<FinishInfo> = None;
        // 先到：带 timings 的 finish chunk
        merge_finish_info(
            &mut slot,
            FinishInfo {
                reason: "stop".to_string(),
                usage: None,
                timings: Some(Timings {
                    predicted_n: 10,
                    predicted_ms: 500.0,
                    predicted_per_second: 20.0,
                    ..Default::default()
                }),
                first_token_at: None,
            },
        );
        // 后到：只带 usage 的尾 chunk
        merge_finish_info(
            &mut slot,
            FinishInfo {
                reason: String::new(),
                usage: Some(usage(6, 10)),
                timings: None,
                first_token_at: None,
            },
        );

        let info = slot.expect("合并后应有结束信息");
        assert_eq!(info.reason, "stop", "后到的空 reason 不应覆盖已有值");
        assert!(info.usage.is_some(), "usage 应被合并进来");
        let timings = info.timings.expect("timings 不应被后续 usage 覆盖丢失");
        assert!((timings.predicted_per_second - 20.0).abs() < 0.001);
    }

    /// 跨层回归：前端用 camelCase 键 `reasoningContent` 传递推理链，
    /// 必须能被反序列化并透传到请求体中。
    ///
    /// 之前后端字段为 snake_case 且无 alias，serde 会静默忽略该字段，
    /// 导致“多轮透传推理链”实际上从未生效。
    #[test]
    fn test_reasoning_content_camel_case_is_passed_through() {
        let json = serde_json::json!([
            { "role": "user", "content": "第一轮提问" },
            { "role": "assistant", "content": "第一轮回答", "reasoningContent": "推理链" },
            { "role": "assistant", "content": "无推理链的回答" }
        ]);
        let messages: Vec<crate::types::ChatMessage> =
            serde_json::from_value(json).expect("前端消息应能反序列化");

        assert_eq!(messages[1].reasoning_content.as_deref(), Some("推理链"));
        assert!(messages[2].reasoning_content.is_none());

        // 转换后必须真正进入发往 API 的消息体中
        let converted = convert_messages(&messages);
        assert_eq!(converted[1].reasoning_content.as_deref(), Some("推理链"));
        assert!(converted[0].reasoning_content.is_none(), "user 消息不携带推理链");
    }
}
