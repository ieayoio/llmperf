//! 压测与性能指标计算
//!
//! 提供与压测场景相关的通用口径，便于不同项目复用同一套统计方法：
//! - **TTFT**：发起请求到首个增量（思考或正文）到达
//! - **首字延迟**：发起请求到首个「正文」增量到达
//! - **TPOT / ITL**：相邻分片间隔的均值与分位数
//! - **token/s**：优先使用服务端 `timings`，否则用 `usage` + 时间戳反推

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::response::{Timings, Usage};
use crate::stream::StreamEvent;

/// 判定 token 速度所需的最小阶段时长（毫秒）
///
/// 阶段时长过短时（例如所有 token 在一个 chunk 里一次性到达），
/// `tokens / duration` 会被放大成毫无参考价值的天文数字，直接视为无数据。
pub const MIN_PHASE_DURATION_MS: u128 = 10;

/// 单个请求默认保留的增量时间戳上限
pub const DEFAULT_MAX_ITL_SAMPLES: usize = 8192;

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

/// 分位数（线性插值，与 numpy.percentile 默认口径一致）
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let idx = (sorted.len() - 1) as f64 * p;
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (idx - lo as f64)
}

/// 由增量到达时刻计算平均分片间隔（TPOT）与 ITL 分位数
///
/// - `tpot_ms`：相邻增量间隔的均值，与 TPOT（Time Per Output Token）同义；
///   服务端一个分片只含一个 token 时它就是真正的 TPOT。
/// - `itl`：间隔的 p50/p95/max，用来发现「先快后卡」的抖动。
///
/// 增量少于 2 个时无法形成间隔，两者返回 `None`。间隔为 0 的样本会被保留：
/// 同一毫秒内到达多个分片是真实的服务端行为，剔除会让 p50 虚高。
pub fn calc_itl_stats(delta_at_ms: &[u128]) -> (Option<f64>, Option<ItlStats>) {
    if delta_at_ms.len() < 2 {
        return (None, None);
    }

    let mut gaps: Vec<f64> = delta_at_ms
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]) as f64)
        .collect();
    let tpot = gaps.iter().sum::<f64>() / gaps.len() as f64;

    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let itl = ItlStats {
        p50_ms: percentile(&gaps, 0.5),
        p95_ms: percentile(&gaps, 0.95),
        max_ms: *gaps.last().unwrap_or(&0.0),
    };

    (Some(tpot), Some(itl))
}

/// 通用 token 速度计算：tokens / 秒；零 token 或窗口过短时返回 `None`
pub fn tokens_per_second(tokens: u64, duration_ms: u128) -> Option<f64> {
    if tokens == 0 || duration_ms < MIN_PHASE_DURATION_MS {
        return None;
    }
    let tps = tokens as f64 / (duration_ms as f64 / 1000.0);
    (tps > 0.0 && tps.is_finite()).then_some(tps)
}

/// 根据 timings / usage 计算 token/s
///
/// ## 数据源优先级
/// 1. **timings**（vLLM 等 API 在最后一个流式 chunk 给出，权威）
///    - `prompt_tps = timings.prompt_per_second`（基于 `prompt_n`，已扣除 cache 命中）
///    - `completion_tps = timings.predicted_per_second`
/// 2. **退化路径**（timings 缺失）：用 `usage` + 时间戳反推
///    - `completion_tps = usage.completion_tokens / (总耗时 - prefill)`
///    - `prompt_tps = usage.uncached_prompt_tokens() / prefill`，**仅在能区分 prefill 时**返回，
///      否则 `None` —— 否则 `prompt_tokens / total` 会得到与 completion_tps 几乎一样的数值。
///
/// ## 按字段独立回退
/// timings 中 prompt / completion 两项互相独立：例如 prefix cache 全命中时
/// `prompt_n = 0`（prompt 项无效），但 completion 项仍然可用；此时只对缺失项退化。
///
/// ## 为什么 prompt_tps 不能简单用 usage.prompt_tokens
/// `usage.prompt_tokens` 通常包含 `prompt_tokens_details.cached_tokens`（cache 命中不计费），
/// 而 `timings.prompt_n` 才是真实参与计算的 token 数。两者直接相除会得到 4-5 倍的偏高估计。
///
/// ## 为什么锚点必须是「首个增量」而不是「首个正文」
/// 推理模型会先输出很长的思考内容再输出正文，而 `usage.completion_tokens`
/// 已包含思考 token。若用首个正文时刻切分，会把整段思考生成时间算进 prefill，
/// 导致 `completion_tps` 高估（实测可达 2~5 倍）、`prompt_tps` 低估。
///
/// # 参数
/// - `timings`: API 在最后一个 chunk 给出的真实速度字段（vLLM 兼容）
/// - `usage`: API 给出的 token 使用量（兜底数据源）
/// - `total_duration_ms`: 整个请求的总耗时，包含网络握手、prefill、补全阶段
/// - `first_token_at_ms`: 首个增量（思考或正文均可）到达的时间点，`None` 表示无法区分 prefill
///
/// # 返回
/// - `.0` 补全阶段 token/s
/// - `.1` Prompt 阶段 token/s（无法可靠计算时为 `None`）
pub fn calc_tokens_per_second(
    timings: Option<&Timings>,
    usage: Option<&Usage>,
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
    let prompt_tps = timings_prompt.or_else(|| match prefill_ms {
        Some(prefill) => tokens_per_second(usage.uncached_prompt_tokens(), prefill),
        None => None,
    });

    (completion_tps, prompt_tps)
}

/// 流式请求的计时器
///
/// 在事件循环里逐个 `observe` 即可得到 TTFT、首字延迟与分片间隔样本，
/// 结束后一次性算出 ITL / TPOT / token 速度。
#[derive(Debug, Clone)]
pub struct StreamTiming {
    start: Instant,
    max_samples: usize,
    delta_at_ms: Vec<u128>,
    /// 首个增量（思考或正文）到达时刻（毫秒）
    pub first_token_ms: Option<u128>,
    /// 首个正文增量到达时刻（毫秒）
    pub first_content_ms: Option<u128>,
}

impl StreamTiming {
    /// 从请求发起时刻开始计时
    pub fn new(start: Instant) -> Self {
        Self {
            start,
            max_samples: DEFAULT_MAX_ITL_SAMPLES,
            delta_at_ms: Vec::new(),
            first_token_ms: None,
            first_content_ms: None,
        }
    }

    /// 设置增量样本上限（达到上限后不再记录新样本）
    pub fn with_max_samples(mut self, max_samples: usize) -> Self {
        self.max_samples = max_samples;
        self
    }

    /// 记录一个事件（使用当前时刻）
    pub fn observe(&mut self, event: &StreamEvent) {
        let at_ms = self.start.elapsed().as_millis();
        self.observe_at(event, at_ms);
    }

    /// 在指定时刻记录一个事件
    pub fn observe_at(&mut self, event: &StreamEvent, at_ms: u128) {
        match event {
            StreamEvent::ReasoningDelta { .. } => {
                self.record(at_ms);
                if self.first_token_ms.is_none() {
                    self.first_token_ms = Some(at_ms);
                }
            }
            StreamEvent::ContentDelta { .. } => {
                self.record(at_ms);
                if self.first_token_ms.is_none() {
                    self.first_token_ms = Some(at_ms);
                }
                if self.first_content_ms.is_none() {
                    self.first_content_ms = Some(at_ms);
                }
            }
            // 结束事件与工具调用不参与分片间隔统计：
            // 前者不是 token，后者与「出字节奏」无关
            _ => {}
        }
    }

    /// 请求总耗时（毫秒）
    pub fn total_ms(&self) -> u128 {
        self.start.elapsed().as_millis()
    }

    /// 已记录的增量到达时刻
    pub fn delta_samples(&self) -> &[u128] {
        &self.delta_at_ms
    }

    /// 平均分片间隔与 ITL 分位数
    pub fn itl(&self) -> (Option<f64>, Option<ItlStats>) {
        calc_itl_stats(&self.delta_at_ms)
    }

    /// 用「消费端记录的首 token 时刻」计算 token 速度
    pub fn tokens_per_second(
        &self,
        timings: Option<&Timings>,
        usage: Option<&Usage>,
    ) -> (Option<f64>, Option<f64>) {
        self.tokens_per_second_at(timings, usage, self.first_token_ms)
    }

    /// 指定 prefill 锚点计算 token 速度
    ///
    /// 建议优先传入解析层打点的首 token 时刻（更接近字节真实到达）。
    pub fn tokens_per_second_at(
        &self,
        timings: Option<&Timings>,
        usage: Option<&Usage>,
        first_token_at_ms: Option<u128>,
    ) -> (Option<f64>, Option<f64>) {
        calc_tokens_per_second(timings, usage, self.total_ms(), first_token_at_ms)
    }

    /// 记录一次增量；达到样本上限后停止记录（已记录部分仍可统计）
    fn record(&mut self, at_ms: u128) {
        if self.delta_at_ms.len() < self.max_samples {
            self.delta_at_ms.push(at_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::PromptTokensDetails;

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
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(cached),
                audio_tokens: None,
            }),
            ..usage(prompt, completion)
        }
    }

    /// 能区分 prefill 时长时，prompt_tps 用 prefill 时间反推
    #[test]
    fn test_calc_tokens_per_second_with_prefill() {
        let u = usage(30, 150);
        let (comp, prompt) = calc_tokens_per_second(None, Some(&u), 1800, Some(300));
        assert!((comp.unwrap() - 100.0).abs() < 0.01);
        let wrong = 150.0 / 1.8;
        assert!(
            (prompt.unwrap() - wrong).abs() > 1.0,
            "prompt_tps 不应再用总耗时推算"
        );
    }

    /// 无 prefill 数据时 prompt_tps 必须为 None，completion_tps 退回总耗时
    #[test]
    fn test_calc_tokens_per_second_no_prefill() {
        let u = usage(30, 150);
        let (comp, prompt) = calc_tokens_per_second(None, Some(&u), 1800, None);
        assert!(comp.is_some());
        assert!(prompt.is_none());
    }

    /// 无 usage 时两个值都为 None
    #[test]
    fn test_calc_tokens_per_second_no_usage() {
        let (comp, prompt) = calc_tokens_per_second(None, None, 1000, Some(200));
        assert!(comp.is_none());
        assert!(prompt.is_none());
    }

    /// prefill 大于总耗时属于异常，prompt_tps 不输出
    #[test]
    fn test_calc_tokens_per_second_prefill_greater_than_total() {
        let u = usage(10, 100);
        let (comp, prompt) = calc_tokens_per_second(None, Some(&u), 500, Some(800));
        assert!((comp.unwrap() - 200.0).abs() < 0.01);
        assert!(prompt.is_none());
    }

    /// 0 token / 0 ms 不应除零
    #[test]
    fn test_calc_tokens_per_second_zero_tokens() {
        let u = usage(0, 0);
        let (comp, prompt) = calc_tokens_per_second(None, Some(&u), 1000, Some(200));
        assert!(comp.is_none());
        assert!(prompt.is_none());
    }

    /// 有 timings 时必须优先使用服务端的真实速度
    #[test]
    fn test_calc_tokens_per_second_prefer_timings() {
        let timings = Timings {
            prompt_n: 4,
            prompt_ms: 97.702,
            prompt_per_second: 40.94,
            predicted_n: 42,
            predicted_ms: 1087.156,
            predicted_per_second: 38.63,
            ..Default::default()
        };
        let u = usage(18, 42);
        let (comp, prompt) = calc_tokens_per_second(Some(&timings), Some(&u), 1500, Some(97));
        assert!((comp.unwrap() - 38.63).abs() < 0.01);
        assert!((prompt.unwrap() - 40.94).abs() < 0.01);
    }

    /// timings 全为 0 时应退化到 usage 反推
    #[test]
    fn test_calc_tokens_per_second_timings_all_zero_falls_back() {
        let timings = Timings::default();
        let u = usage(30, 150);
        let (comp, prompt) = calc_tokens_per_second(Some(&timings), Some(&u), 1800, Some(300));
        assert!((comp.unwrap() - 100.0).abs() < 0.01);
        assert!((prompt.unwrap() - 100.0).abs() < 0.01);
    }

    /// timings 只有一半时，缺失项独立回退
    #[test]
    fn test_calc_tokens_per_second_partial_timings() {
        let timings = Timings {
            predicted_n: 42,
            predicted_ms: 1000.0,
            predicted_per_second: 42.0,
            ..Default::default()
        };
        let u = usage(30, 42);
        let (comp, prompt) = calc_tokens_per_second(Some(&timings), Some(&u), 1300, Some(300));
        assert!((comp.unwrap() - 42.0).abs() < 0.01);
        assert!((prompt.unwrap() - 100.0).abs() < 0.01);
    }

    /// 退化路径必须扣除 cache 命中 token
    #[test]
    fn test_calc_tokens_per_second_deducts_cached_tokens() {
        let u = usage_with_cache(100, 90, 50);
        let (_, prompt) = calc_tokens_per_second(None, Some(&u), 1200, Some(200));
        assert!((prompt.unwrap() - 50.0).abs() < 0.01);
    }

    /// 思考模型：completion 窗口必须从首个思考 token 开始算
    #[test]
    fn test_calc_tokens_per_second_reasoning_anchor() {
        let u = usage(20, 100);
        let (comp, _) = calc_tokens_per_second(None, Some(&u), 1000, Some(200));
        assert!((comp.unwrap() - 125.0).abs() < 0.01);
        let (comp_wrong, _) = calc_tokens_per_second(None, Some(&u), 1000, Some(998));
        assert!(comp_wrong.is_none(), "过短窗口应被过滤");
    }

    /// 等间隔分片应得到一致的均值/中位数/分位数
    #[test]
    fn test_calc_itl_stats_uniform_gaps() {
        let (tpot, itl) = calc_itl_stats(&[100, 120, 140, 160, 180]);
        assert!((tpot.unwrap() - 20.0).abs() < 1e-9);
        let itl = itl.unwrap();
        assert!((itl.p50_ms - 20.0).abs() < 1e-9);
        assert!((itl.p95_ms - 20.0).abs() < 1e-9);
        assert!((itl.max_ms - 20.0).abs() < 1e-9);
    }

    /// 抖动场景：长间隔必须体现在 max / P95 上，中位数不被带偏
    #[test]
    fn test_calc_itl_stats_captures_stall() {
        let (tpot, itl) = calc_itl_stats(&[0, 10, 20, 920]);
        let itl = itl.unwrap();
        assert!((tpot.unwrap() - (920.0 / 3.0)).abs() < 1e-9);
        assert!((itl.max_ms - 900.0).abs() < 1e-9);
        assert!(itl.p95_ms > 800.0);
        assert!(itl.p50_ms < 20.0);
    }

    /// 少于 2 个增量无法形成间隔
    #[test]
    fn test_calc_itl_stats_needs_two_samples() {
        assert_eq!(calc_itl_stats(&[]), (None, None));
        assert_eq!(calc_itl_stats(&[42]), (None, None));
    }

    /// 同一毫秒到达多个分片时 gap=0 应保留
    #[test]
    fn test_calc_itl_stats_keeps_zero_gaps() {
        let (tpot, itl) = calc_itl_stats(&[5, 5, 5]);
        assert_eq!(tpot, Some(0.0));
        assert_eq!(itl.unwrap().p50_ms, 0.0);
    }

    /// 分位数线性插值口径
    #[test]
    fn test_percentile_linear_interpolation() {
        let sorted = [10.0, 20.0, 30.0, 40.0];
        assert!((percentile(&sorted, 0.0) - 10.0).abs() < 1e-9);
        assert!((percentile(&sorted, 0.5) - 25.0).abs() < 1e-9);
        assert!((percentile(&sorted, 1.0) - 40.0).abs() < 1e-9);
        assert!((percentile(&sorted, 0.95) - 38.5).abs() < 1e-9);
    }

    /// StreamTiming：思考首字与正文首字要分别记录，ITL 只统计文本增量
    #[test]
    fn test_stream_timing_observes_events() {
        let start = Instant::now();
        let mut timing = StreamTiming::new(start);
        timing.observe_at(
            &StreamEvent::ReasoningDelta {
                index: 0,
                text: "想".into(),
            },
            100,
        );
        timing.observe_at(
            &StreamEvent::ContentDelta {
                index: 0,
                text: "你".into(),
            },
            180,
        );
        timing.observe_at(
            &StreamEvent::ContentDelta {
                index: 0,
                text: "好".into(),
            },
            200,
        );
        timing.observe_at(&StreamEvent::Finish(Default::default()), 210);

        assert_eq!(timing.first_token_ms, Some(100));
        assert_eq!(timing.first_content_ms, Some(180));
        assert_eq!(timing.delta_samples().len(), 3, "结束事件不应计入增量");
        let (tpot, itl) = timing.itl();
        assert!((tpot.unwrap() - 50.0).abs() < 1e-9);
        assert!(itl.is_some());
    }

    /// 样本上限生效后不再增长
    #[test]
    fn test_stream_timing_respects_max_samples() {
        let mut timing = StreamTiming::new(Instant::now()).with_max_samples(2);
        for at in 0..10 {
            timing.observe_at(
                &StreamEvent::ContentDelta {
                    index: 0,
                    text: "x".into(),
                },
                at,
            );
        }
        assert_eq!(timing.delta_samples().len(), 2);
    }
}
