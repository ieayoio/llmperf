use serde::{Deserialize, Serialize};

/// 流式增量间隔（ITL，Inter-Token Latency）统计
///
/// 统计对象是「相邻两个流式增量到达的时间差」：服务端一个 SSE 分片可能包含多个 token，
/// 因此该指标反映的是分片粒度下的出字节奏与卡顿，而不是严格的逐 token 延迟。
/// 它比平均 tok/s 更能暴露「先快后卡」这类抖动问题。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct ItlStats {
    /// 分片间隔中位数（毫秒）
    pub p50_ms: f64,
    /// 分片间隔 P95（毫秒）
    pub p95_ms: f64,
    /// 分片间隔最大值（毫秒），用于定位偶发长卡顿
    pub max_ms: f64,
}

/// 单个窗口的最终请求结果
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SingleResult {
    pub window_id: usize,
    pub assistant_content: String,
    /// 思考/推理内容（推理模型会返回）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    pub duration_ms: u128,
    /// 首 token 延迟（毫秒）：从发起请求到首个增量（思考或正文）到达的时间。
    ///
    /// 请求在收到任何增量前就失败时为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u128>,
    /// 正文首字延迟（毫秒）：从发起请求到首个「正文」增量到达的时间。
    ///
    /// 推理模型会先输出很长的思考内容，`ttft_ms` 反映的是首个思考 token，
    /// 只有本字段才是用户真正开始看到答案的时间。无正文增量时为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_content_ms: Option<u128>,
    /// 平均分片间隔（毫秒/分片）
    ///
    /// 即「(末增量 - 首增量) / (增量数 - 1)」，在“一分片一 token”的服务端实现下
    /// 等价于 TPOT（Time Per Output Token）。增量少于 2 个时为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tpot_ms: Option<f64>,
    /// 分片间隔的分位数统计，用于衡量流式输出的平滑度
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itl: Option<ItlStats>,
    /// Prompt token 数（来自 `usage`，无 `usage` 时为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    /// Completion token 数（来自 `usage`，包含思考 token）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    /// 补全阶段 token 速度（每秒 token 数）
    ///
    /// 优先取 API 的 `timings`；无 `timings` 时用 `usage.completion_tokens`
    /// 除以「总耗时 - 首个增量到达时刻」反推，无法可靠计算时为 None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens_per_second: Option<f64>,
    /// Prompt 阶段 token 速度（每秒 token 数）
    ///
    /// 优先取 API 的 `timings`；无 `timings` 且无法区分 prefill 阶段时为 None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_per_second: Option<f64>,
    /// 是否被用户主动取消
    ///
    /// 用结构化字段而非 `error` 文案判别，避免展示文案调整后前端误判状态。
    #[serde(default)]
    pub canceled: bool,
    pub error: Option<String>,
}

/// 流式 chunk 事件
///
/// 该事件通过 Tauri `emit` 广播到所有 webview 窗口。前端收到后必须按 `target_window_id`
/// 严格过滤，仅当 `event.payload.target_window_id === window.id` 时才消费。
/// 后端目前只有一个 webview 窗口（主窗口），如未来拆分为多 webview，应改用
/// `Emitter::emit_to(label, ...)` 仅发给对应的 webview。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StreamChunkEvent {
    /// 目标 React 窗口 ID（前端 `ChatWindow.id`），用于前端过滤。
    pub target_window_id: usize,
    /// 正文回复增量（非空时表示此事件为正文内容）
    pub content: String,
    /// 思考/推理内容增量（非空时表示此事件为思考过程）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    pub finished: bool,
    pub error: Option<String>,
    pub duration_ms: u128,
    /// 首 token 延迟（毫秒），仅在完成事件中携带
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u128>,
    /// 正文首字延迟（毫秒），仅在完成事件中携带
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_content_ms: Option<u128>,
    /// 平均分片间隔（毫秒/分片），仅在完成事件中携带
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tpot_ms: Option<f64>,
    /// 分片间隔分位数统计，仅在完成事件中携带
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itl: Option<ItlStats>,
    /// 补全阶段 token 速度（每秒 token 数，无数据时为 null）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens_per_second: Option<f64>,
    /// Prompt 阶段 token 速度（每秒 token 数，无数据时为 null）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_per_second: Option<f64>,
    /// 是否被用户主动取消（前端据此设置 canceled 状态，不依赖 error 文案）
    #[serde(default)]
    pub canceled: bool,
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
    /// 请求超时（秒），不传则使用后端默认值
    ///
    /// reqwest 的超时覆盖整个请求生命周期，推理模型长输出容易超过 2 分钟，
    /// 因此需要允许调用方覆盖默认值。
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// 最大生成 token 数，None 表示使用服务端默认值
    ///
    /// 压测时必须固定输出长度，否则不同轮次的 tok/s 无法横向比较。
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// 采样温度，None 表示使用服务端默认值
    ///
    /// 可复现性测试建议设为 0。
    #[serde(default)]
    pub temperature: Option<f32>,
    /// 核采样概率 [0.0, 1.0]，与 temperature 二选一使用，None 表示使用服务端默认值
    #[serde(default)]
    pub top_p: Option<f32>,
    /// 频率惩罚 [-2.0, 2.0]，正值抑制重复用词，None 表示使用服务端默认值
    #[serde(default)]
    pub frequency_penalty: Option<f32>,
    /// 存在惩罚 [-2.0, 2.0]，正值鼓励模型引入新话题，None 表示使用服务端默认值
    #[serde(default)]
    pub presence_penalty: Option<f32>,
    /// 随机种子，配置后采样结果尽量可复现（需服务端支持），None 表示不传递
    #[serde(default)]
    pub seed: Option<i64>,
    /// 静默模式：不向前端推送任何 `stream_chunk` 事件，结果仅通过命令返回值回传
    ///
    /// 并发梯度扫描时同时会有几十路请求，逐 token 推送会让 webview 频繁重渲染，
    /// 既拖慢 UI 也影响压测时的计时精度，因此扫描模式必须走静默通道。
    #[serde(default)]
    pub silent: bool,
}

/// 单条聊天消息
#[derive(Deserialize, Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// 思考/推理内容（推理模型在多轮对话时要求携带上一轮 assistant 的推理链）
    ///
    /// 前端使用 camelCase 键 `reasoningContent` 传递，这里通过 alias 兼容，
    /// 否则 serde 会静默忽略该字段，导致推理链永远不会被透传。
    #[serde(default, alias = "reasoningContent")]
    pub reasoning_content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_result() -> SingleResult {
        SingleResult {
            window_id: 1,
            assistant_content: String::new(),
            reasoning_content: None,
            duration_ms: 1200,
            ttft_ms: Some(320),
            first_content_ms: Some(400),
            tpot_ms: Some(18.5),
            itl: Some(ItlStats {
                p50_ms: 17.0,
                p95_ms: 31.0,
                max_ms: 42.0,
            }),
            prompt_tokens: Some(10),
            completion_tokens: Some(50),
            completion_tokens_per_second: Some(41.6),
            prompt_tokens_per_second: None,
            canceled: false,
            error: None,
        }
    }

    /// 前端按 snake_case 字段名解析结果，序列化后不能变成 camelCase
    #[test]
    fn test_single_result_serializes_snake_case() {
        let json = serde_json::to_value(sample_result()).unwrap();
        assert_eq!(json["ttft_ms"], 320);
        assert_eq!(json["first_content_ms"], 400);
        assert_eq!(json["itl"]["p95_ms"], 31.0);
        assert_eq!(json["prompt_tokens"], 10);
        assert_eq!(json["completion_tokens"], 50);
        assert!(json.get("prompt_tokens_per_second").is_none(), "None 字段应被省略");
        // 取消态字段必须总是输出（前端用 === true 判断，不依赖是否存在）
        assert_eq!(json["canceled"], false);
    }

    /// 非推理模型没有思考阶段时，正文首字与首 token 可同时存在，
    /// 但都缺失时应被省略而不是序列化成 null（前端按 undefined 判定无数据）
    #[test]
    fn test_single_result_omits_missing_latency_fields() {
        let mut result = sample_result();
        result.first_content_ms = None;
        result.tpot_ms = None;
        result.itl = None;

        let json = serde_json::to_value(result).unwrap();
        assert!(json.get("first_content_ms").is_none());
        assert!(json.get("tpot_ms").is_none());
        assert!(json.get("itl").is_none());
    }

    /// 前端未配置采样参数时传 null，必须解析为 None（使用服务端默认值）
    #[test]
    fn test_request_config_accepts_null_sampling_params() {
        let json = serde_json::json!({
            "base_url": "http://127.0.0.1:16777/v1",
            "api_key": "sk-test",
            "model": "testllm",
            "messages": [],
            "window_id": 0,
            "max_tokens": null,
            "temperature": null
        });
        let config: LLMRequestConfig = serde_json::from_value(json).unwrap();
        assert!(config.max_tokens.is_none());
        assert!(config.temperature.is_none());
        assert!(config.top_p.is_none());
        assert!(config.frequency_penalty.is_none());
        assert!(config.presence_penalty.is_none());
        assert!(config.seed.is_none());
        assert!(!config.silent, "未传 silent 时应默认为 false（保持原有流式行为）");
    }

    /// 旧版前端不传采样参数时仍可解析（向后兼容）
    #[test]
    fn test_request_config_sampling_params_optional() {
        let json = serde_json::json!({
            "base_url": "http://127.0.0.1:16777/v1",
            "api_key": "sk-test",
            "model": "testllm",
            "messages": [],
            "window_id": 0
        });
        let config: LLMRequestConfig = serde_json::from_value(json).unwrap();
        assert!(config.max_tokens.is_none());
        assert!(config.temperature.is_none());
        assert!(config.top_p.is_none());
        assert!(config.seed.is_none());
    }

    /// 配置采样参数时应正确透传
    #[test]
    fn test_request_config_sampling_params_passed_through() {
        let json = serde_json::json!({
            "base_url": "http://127.0.0.1:16777/v1",
            "api_key": "sk-test",
            "model": "testllm",
            "messages": [],
            "window_id": 3,
            "max_tokens": 256,
            "temperature": 0.0,
            "top_p": 0.9,
            "frequency_penalty": 0.5,
            "presence_penalty": -0.5,
            "seed": 42,
            "silent": true
        });
        let config: LLMRequestConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.max_tokens, Some(256));
        assert_eq!(config.temperature, Some(0.0));
        assert_eq!(config.top_p, Some(0.9));
        assert_eq!(config.frequency_penalty, Some(0.5));
        assert_eq!(config.presence_penalty, Some(-0.5));
        assert_eq!(config.seed, Some(42));
        assert!(config.silent, "梯度扫描依赖 silent 关闭逐 token 推送");
    }
}
