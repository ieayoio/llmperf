//! 单次流式请求的执行与指标汇总
//!
//! 本模块是「通用 llm-client」与「压测界面」之间的适配层：
//! - 把前端配置转成客户端参数
//! - 把流式事件转发给 webview（可选静默）
//! - 用库提供的指标工具算出 TTFT / TPOT / ITL / token 速度

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use llm_client::{
    ChatMessage, ChatParams, ClientConfig, ItlStats, LlmClient, LlmError, RequestOptions,
    StreamAccumulator, StreamEvent, StreamTiming,
};
use tauri::Emitter;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::types::{LLMRequestConfig, SingleResult, StreamChunkEvent};

/// 用户主动取消时的统一文案
///
/// 目前同时作为前后端约定的判别值（前端据 `error` 内容识别取消态），
/// 修改时必须同步调整 `src/App.tsx`。结构化标记见 `StreamChunkEvent.canceled`。
const CANCELED_MESSAGE: &str = "用户已取消";

/// 默认请求超时（秒）
///
/// 推理模型的长输出很容易超过 2 分钟，因此默认放宽到 10 分钟；
/// 前端可通过 `LLMRequestConfig.timeout_secs` 覆盖。
const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// 默认连接超时（秒）
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

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
async fn get_client(
    base_url: String,
    api_key: String,
    timeout_secs: u64,
) -> llm_client::Result<LlmClient> {
    let key = ClientKey {
        base_url: base_url.clone(),
        api_key: api_key.clone(),
        timeout_secs,
    };

    let mut cache = client_cache().lock().await;
    if let Some(client) = cache.get(&key) {
        return Ok(client.clone());
    }

    let timeout = Duration::from_secs(timeout_secs);
    let config = ClientConfig::new(base_url, api_key)
        .with_connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
        // 等待响应头：覆盖建连 + prefill
        .with_response_timeout(timeout)
        // 流中途长时间无数据视为失败（reqwest 的总超时会误杀长流，这里改成空闲超时）
        .with_stream_idle_timeout(Some(timeout))
        // 非流式响应体的读取超时
        .with_total_timeout(Some(timeout))
        // 压测不重试：重试会把两次请求的耗时叠加，污染延迟与吞吐统计
        .with_max_retries(0);

    // 创建过程是同步的（不发起网络请求），持锁时间极短
    let client = LlmClient::new(config)?;
    // 超出上限时整体清空：桌面工具场景缓存条目极少，无需精细淘汰策略
    if cache.len() >= CLIENT_CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key, client.clone());
    Ok(client)
}

// ============================ 事件出口 ============================

/// 流式事件发射器
///
/// 封装 `silent` 开关：静默模式（并发梯度扫描）下不推送任何 `stream_chunk` 事件，
/// 结果仅通过命令返回值回传。否则几十路并发逐 token 推送会让 webview 频繁重渲染，
/// 既拖慢界面，也会污染压测计时。
struct EventSink {
    app: tauri::AppHandle,
    window_id: usize,
    silent: bool,
}

impl EventSink {
    fn new(app: tauri::AppHandle, window_id: usize, silent: bool) -> Self {
        Self {
            app,
            window_id,
            silent,
        }
    }

    /// 统一出口：静默模式直接丢弃
    fn emit(&self, event: StreamChunkEvent) {
        if self.silent {
            return;
        }
        let _ = self.app.emit("stream_chunk", event);
    }

    /// 事件模板：只填公共字段，调用方再用结构体更新语法覆盖差异字段
    fn template(&self, finished: bool, duration_ms: u128) -> StreamChunkEvent {
        StreamChunkEvent {
            target_window_id: self.window_id,
            content: String::new(),
            reasoning_content: None,
            finished,
            error: None,
            duration_ms,
            ttft_ms: None,
            first_content_ms: None,
            tpot_ms: None,
            itl: None,
            completion_tokens_per_second: None,
            prompt_tokens_per_second: None,
            canceled: false,
        }
    }

    /// 错误完成事件
    fn error(&self, duration_ms: u128, error: String) {
        self.emit(StreamChunkEvent {
            error: Some(error),
            ..self.template(true, duration_ms)
        });
    }

    /// 正文流式增量
    fn chunk(&self, start: Instant, chunk: &str) {
        self.emit(StreamChunkEvent {
            content: chunk.to_string(),
            ..self.template(false, start.elapsed().as_millis())
        });
    }

    /// 思考内容流式增量
    fn reasoning(&self, start: Instant, chunk: &str) {
        self.emit(StreamChunkEvent {
            reasoning_content: Some(chunk.to_string()),
            ..self.template(false, start.elapsed().as_millis())
        });
    }

    /// 成功完成事件：携带全部延迟指标
    fn finished(&self, metrics: &LatencyMetrics) {
        self.emit(StreamChunkEvent {
            ttft_ms: metrics.ttft_ms,
            first_content_ms: metrics.first_content_ms,
            tpot_ms: metrics.tpot_ms,
            itl: metrics.itl,
            completion_tokens_per_second: metrics.completion_tps,
            prompt_tokens_per_second: metrics.prompt_tps,
            ..self.template(true, metrics.duration_ms)
        });
    }

    /// 用户取消事件：复用 finished 通道，前端走现有 error 分支
    fn canceled(&self, metrics: &LatencyMetrics) {
        self.emit(StreamChunkEvent {
            // 用统一的"用户已取消"文案，便于前端展示
            error: Some(CANCELED_MESSAGE.to_string()),
            // 结构化标记：前端据这个字段设置取消态，不依赖 error 文案
            canceled: true,
            ..self.template(true, metrics.duration_ms)
        });
    }
}

/// 单次请求的延迟指标集合
///
/// 统一在流结束时算好，再分别用于推送完成事件与构造命令返回值，
/// 避免两处各算一遍导致数值不一致。
#[derive(Default, Clone, Copy)]
struct LatencyMetrics {
    duration_ms: u128,
    ttft_ms: Option<u128>,
    first_content_ms: Option<u128>,
    tpot_ms: Option<f64>,
    itl: Option<ItlStats>,
    completion_tps: Option<f64>,
    prompt_tps: Option<f64>,
}

/// 构造最终返回结果：统一填字段，避免每个分支各写一遍十几个字段
fn build_result(
    window_id: usize,
    metrics: &LatencyMetrics,
    assistant_content: String,
    reasoning_content: String,
    canceled: bool,
    error: Option<String>,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
) -> SingleResult {
    SingleResult {
        window_id,
        assistant_content,
        // 空思考内容不写入，避免前端把空串当成"有推理链"
        reasoning_content: (!reasoning_content.is_empty()).then_some(reasoning_content),
        duration_ms: metrics.duration_ms,
        ttft_ms: metrics.ttft_ms,
        first_content_ms: metrics.first_content_ms,
        tpot_ms: metrics.tpot_ms,
        itl: metrics.itl,
        prompt_tokens,
        completion_tokens,
        completion_tokens_per_second: metrics.completion_tps,
        prompt_tokens_per_second: metrics.prompt_tps,
        canceled,
        error,
    }
}

/// 将前端传入的 `ChatMessage` (简单结构) 转换为 `llm_client` 的消息模型
///
/// 透传 `reasoning_content` 字段：部分推理模型（如 deepseek-reasoner）在多轮对话时
/// 要求 assistant 历史消息携带上一轮的推理链，否则会校验失败或答复降级。
fn convert_messages(messages: &[crate::types::ChatMessage]) -> Vec<ChatMessage> {
    messages
        .iter()
        .map(|m| match m.role.to_lowercase().as_str() {
            "system" => ChatMessage::system(&m.content),
            "assistant" => match m.reasoning_content.as_deref().filter(|r| !r.is_empty()) {
                Some(reasoning) => ChatMessage::assistant_with_reasoning(&m.content, reasoning),
                None => ChatMessage::assistant(&m.content),
            },
            // 压测界面只做纯文本对话，未知角色按 user 处理
            _ => ChatMessage::user(&m.content),
        })
        .collect()
}

/// 执行单次流式请求，返回最终结果
///
/// ## 取消语义
/// 取消信号通过 `RequestOptions::cancel` 传给客户端：
/// - 建连阶段取消：`chat_stream_with` 直接返回 [`LlmError::Canceled`]
/// - 生成阶段取消：流返回 `Err(Canceled)`，已累积的正文/思考内容会保留
///
/// ## 静默模式
/// `config.silent = true` 时不推送任何流式事件（包括完成/错误事件），
/// 调用方只能从返回值拿到结果。梯度扫描就是靠这个模式避免几十路并发刷爆前端。
pub async fn execute_stream_request(
    app: tauri::AppHandle,
    window_id: usize,
    config: LLMRequestConfig,
    cancel_token: CancellationToken,
) -> SingleResult {
    let start = Instant::now();
    // 事件出口：集中控制静默开关
    let sink = EventSink::new(app, window_id, config.silent);
    let timeout_secs = config.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);

    // 1. 获取客户端（按 base_url / api_key / 超时复用连接池）
    let client = match get_client(
        config.base_url.trim().to_string(),
        config.api_key.trim().to_string(),
        timeout_secs,
    )
    .await
    {
        Ok(client) => client,
        Err(e) => {
            let error_msg = format!("客户端初始化失败: {e}");
            let metrics = LatencyMetrics {
                duration_ms: start.elapsed().as_millis(),
                ..Default::default()
            };
            sink.error(metrics.duration_ms, error_msg.clone());
            return build_result(
                window_id,
                &metrics,
                String::new(),
                String::new(),
                false,
                Some(error_msg),
                None,
                None,
            );
        }
    };

    // 2. 转换消息格式与模型参数（采样参数由前端配置，未配置时交给服务端默认值）
    let messages = convert_messages(&config.messages);
    let params = ChatParams {
        model: config.model.trim().to_string(),
        max_tokens: config.max_tokens,
        temperature: config.temperature,
        top_p: config.top_p,
        frequency_penalty: config.frequency_penalty,
        presence_penalty: config.presence_penalty,
        seed: config.seed,
        ..Default::default()
    };

    // 3. 发送流式请求（建连与等待响应头在此阶段完成，取消信号已参与 select）
    let options = RequestOptions::new().with_cancel(cancel_token);
    let mut stream = match client.chat_stream_with(&messages, &params, options).await {
        Ok(stream) => stream,
        Err(LlmError::Canceled) => {
            // 建连阶段就收到取消：SSE 流尚未建立，没有任何延迟指标
            let metrics = LatencyMetrics {
                duration_ms: start.elapsed().as_millis(),
                ..Default::default()
            };
            sink.canceled(&metrics);
            return build_result(
                window_id,
                &metrics,
                String::new(),
                String::new(),
                true,
                Some(CANCELED_MESSAGE.to_string()),
                None,
                None,
            );
        }
        Err(e) => {
            let error_msg = format!("流式请求发送失败: {e}");
            let metrics = LatencyMetrics {
                duration_ms: start.elapsed().as_millis(),
                ..Default::default()
            };
            sink.error(metrics.duration_ms, error_msg.clone());
            return build_result(
                window_id,
                &metrics,
                String::new(),
                String::new(),
                false,
                Some(error_msg),
                None,
                None,
            );
        }
    };

    // 4. 逐事件处理流式响应
    //    - 累加器负责内容聚合与多次结束信息的按字段合并
    //    - 计时器负责 TTFT / 首字延迟 / 分片间隔
    let mut accumulator = StreamAccumulator::default();
    let mut timing = StreamTiming::new(start);
    let mut last_error: Option<String> = None;
    let mut canceled = false;

    while let Some(event) = stream.recv().await {
        match event {
            Ok(event) => {
                timing.observe(&event);
                accumulator.feed(&event);
                match &event {
                    StreamEvent::ReasoningDelta { text, .. } if !text.is_empty() => {
                        sink.reasoning(start, text)
                    }
                    StreamEvent::ContentDelta { text, .. } if !text.is_empty() => {
                        sink.chunk(start, text)
                    }
                    // 压测界面不展示工具调用；结束事件由下方统一处理
                    _ => {}
                }
            }
            Err(LlmError::Canceled) => {
                canceled = true;
                break;
            }
            Err(e) => {
                last_error = Some(format!("流式请求失败: {e}"));
                break;
            }
        }
    }

    // 5. 计算各项指标
    let total_duration_ms = timing.total_ms();
    // prefill 锚点：优先用 SSE 解析层打点的首 token 时刻（更接近字节真实到达），
    // 解析层拿不到时（例如流被截断）退回消费端记录的时刻
    let prefill_at_ms = accumulator
        .first_token_at
        .map(|at| at.saturating_duration_since(start).as_millis())
        .or(timing.first_token_ms);
    let (completion_tps, prompt_tps) = timing.tokens_per_second_at(
        accumulator.timings.as_ref(),
        accumulator.usage.as_ref(),
        prefill_at_ms,
    );
    // 流式平滑度：以增量到达时刻统计分片间隔（含思考分片，与 TTFT 口径保持一致）
    let (tpot_ms, itl) = timing.itl();

    // 从 usage 提取 token 数：0 视为服务端未提供，避免汇总时被无意义地拉低
    let usage = accumulator.usage.as_ref();
    let prompt_tokens = usage.and_then(|u| (u.prompt_tokens > 0).then_some(u.prompt_tokens));
    let completion_tokens =
        usage.and_then(|u| (u.completion_tokens > 0).then_some(u.completion_tokens));

    let metrics = LatencyMetrics {
        duration_ms: total_duration_ms,
        ttft_ms: prefill_at_ms,
        first_content_ms: timing.first_content_ms,
        tpot_ms,
        itl,
        completion_tps,
        prompt_tps,
    };
    // 取消 / 出错时样本不完整：速度与间隔统计一律不输出，避免污染汇总。
    // 首字类指标仍然有效，保留下来便于定位"首字慢"类问题。
    let partial_metrics = LatencyMetrics {
        completion_tps: None,
        prompt_tps: None,
        tpot_ms: None,
        itl: None,
        ..metrics
    };

    // 调试日志：打印本次请求的关键指标
    eprintln!(
        "[llmperf] window={window_id} total_ms={total_duration_ms} prefill_ms={prefill_at_ms:?} content_ms={:?} tpot={tpot_ms:?} completion_tps={completion_tps:?} prompt_tps={prompt_tps:?}",
        timing.first_content_ms
    );

    let content = accumulator.content().to_string();
    let reasoning = accumulator.reasoning_content().to_string();

    // 6. 取消：发送统一的"用户已取消"完成事件，不算错误
    if canceled {
        sink.canceled(&partial_metrics);
        return build_result(
            window_id,
            &partial_metrics,
            content,
            reasoning,
            true,
            Some(CANCELED_MESSAGE.to_string()),
            // 取消时 usage 通常不完整，不输出 token 数，避免汇总时被低估
            None,
            None,
        );
    }

    // 7. 异常收敛：解析失败、或流未收到完成标记（既无 finish_reason 也无 [DONE]）
    //    都必须告知前端，否则半截内容会被当成完整结果展示
    let error = last_error
        .or_else(|| (!accumulator.is_complete()).then(|| "流意外结束：未收到完成标记".to_string()));
    if let Some(err) = error {
        sink.error(total_duration_ms, err.clone());
        return build_result(
            window_id,
            &partial_metrics,
            content,
            reasoning,
            false,
            Some(err),
            prompt_tokens,
            completion_tokens,
        );
    }

    // 8. 正常完成：发送携带全部延迟指标的完成事件
    sink.finished(&metrics);

    build_result(
        window_id,
        &metrics,
        content,
        reasoning,
        false,
        None,
        prompt_tokens,
        completion_tokens,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 跨层回归：前端用 camelCase 键 `reasoningContent` 传递推理链，
    /// 必须能被反序列化并透传到请求体中。
    ///
    /// 之前后端字段为 snake_case 且无 alias，serde 会静默忽略该字段，
    /// 导致"多轮透传推理链"实际上从未生效。
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

        let converted = convert_messages(&messages);
        assert_eq!(converted[1].reasoning_content.as_deref(), Some("推理链"));
        assert!(
            converted[0].reasoning_content.is_none(),
            "user 消息不携带推理链"
        );
    }

    /// 消息角色映射：system / assistant / 其他都按预期落到对应角色
    #[test]
    fn test_convert_messages_roles() {
        let messages = vec![
            crate::types::ChatMessage {
                role: "system".into(),
                content: "规则".into(),
                reasoning_content: None,
            },
            crate::types::ChatMessage {
                role: "USER".into(),
                content: "你好".into(),
                reasoning_content: None,
            },
            crate::types::ChatMessage {
                role: "assistant".into(),
                content: "在".into(),
                reasoning_content: Some("思考".into()),
            },
        ];
        let converted = convert_messages(&messages);
        assert_eq!(converted.len(), 3);
        assert_eq!(converted[0].text(), "规则");
        assert_eq!(converted[1].text(), "你好");
        assert_eq!(converted[2].text(), "在");
    }
}
