//! SSE 解码器（纯内存，不做 I/O）
//!
//! 把字节缓冲按行切开、解析 `data:` 负载并转换成 [`StreamEvent`]。
//! 与 I/O 解耦后，超时、取消、调试钩子都可以在上层灵活组合，测试也无需真实网络。

use std::collections::VecDeque;
use std::time::Instant;

use serde::Deserialize;
use serde_json::Value;

use crate::config::DebugHook;
use crate::error::{api_error_from_value, LlmError, Result};
use crate::response::{Timings, Usage};
use crate::stream::{FinishInfo, StreamEvent, ToolCallDelta};

/// 单行缓冲上限（字节）
///
/// 正常 SSE 每行几十到几百字节。若服务端长时间不发送换行（异常实现或恶意端点），
/// 缓冲区会无界增长直到 OOM，因此超过上限时主动中断并报错。
const MAX_SSE_LINE_BYTES: usize = 4 * 1024 * 1024;

/// 错误信息里保留的原始数据长度上限
const MAX_RAW_DATA_IN_ERROR: usize = 512;

/// 单行解析结果
enum LineOutcome {
    /// 解析出若干事件
    Events(Vec<StreamEvent>),
    /// 空行 / 注释 / 非 data 行，跳过
    Skip,
    /// 缓冲区里没有完整行，需要更多数据
    NeedMore,
    /// 收到 `[DONE]`
    Done,
    /// 致命错误：流必须立即终止
    Fatal(LlmError),
}

/// SSE 解码器
pub(crate) struct SseDecoder {
    /// 行缓冲：处理跨 chunk 的不完整行
    buffer: Vec<u8>,
    /// 已解析但尚未派发的事件
    pending: VecDeque<Result<StreamEvent>>,
    /// 首个增量到达时刻，随 Finish 事件带出，用于计算 prefill 耗时
    first_token_at: Option<Instant>,
    /// 是否已收到 `[DONE]` 或致命错误
    done: bool,
    /// 原始负载调试回调
    hook: Option<DebugHook>,
}

impl SseDecoder {
    /// 创建解码器
    pub(crate) fn new(hook: Option<DebugHook>) -> Self {
        Self {
            buffer: Vec::with_capacity(4096),
            pending: VecDeque::new(),
            first_token_at: None,
            done: false,
            hook,
        }
    }

    /// 追加一段字节
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// 流是否已经终止（`[DONE]` 或错误）
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// 数据读取完毕：把「没有尾随换行」的最后一行也纳入解析
    ///
    /// 部分网关的最后一帧不带 `\n`，直接丢弃会静默丢失数据。
    pub(crate) fn flush(&mut self) {
        if !self.buffer.is_empty() {
            self.buffer.push(b'\n');
        }
    }

    /// 取出下一个事件
    ///
    /// 返回 `None` 表示「当前缓冲区已排空」，调用方需要继续读取字节；
    /// 若同时 [`Self::is_done`] 为真，则表示流已彻底结束。
    pub(crate) fn next_event(&mut self) -> Option<Result<StreamEvent>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(event);
            }
            if self.done {
                return None;
            }

            match self.parse_line() {
                LineOutcome::Events(mut events) => {
                    if events.is_empty() {
                        continue;
                    }
                    let first = events.remove(0);
                    self.pending.extend(events.into_iter().map(Ok));
                    return Some(Ok(first));
                }
                LineOutcome::Skip => continue,
                LineOutcome::NeedMore => return None,
                LineOutcome::Done => {
                    // 补发「流正常结束」标记：调用方据此区分
                    // 「服务端确认结束」与「连接被中途截断」（后者不会产生任何结束事件）
                    self.done = true;
                    self.pending
                        .push_back(Ok(StreamEvent::Finish(FinishInfo::done(
                            self.first_token_at,
                        ))));
                }
                LineOutcome::Fatal(err) => {
                    self.done = true;
                    self.pending.clear();
                    return Some(Err(err));
                }
            }
        }
    }

    /// 从缓冲区解析一行
    fn parse_line(&mut self) -> LineOutcome {
        let Some(newline_pos) = self.buffer.iter().position(|&b| b == b'\n') else {
            if self.buffer.len() > MAX_SSE_LINE_BYTES {
                return LineOutcome::Fatal(LlmError::StreamParse(format!(
                    "SSE 单行数据超过 {} MiB，已中断",
                    MAX_SSE_LINE_BYTES / 1024 / 1024
                )));
            }
            return LineOutcome::NeedMore;
        };

        // 逐行消费；`drain` 后缓冲区保留未完成的尾部
        let line_bytes: Vec<u8> = self.buffer.drain(..=newline_pos).collect();
        let line = String::from_utf8_lossy(&line_bytes);
        let line = line.trim();

        // 空行是事件分隔符；`:` 开头是注释/心跳
        if line.is_empty() || line.starts_with(':') {
            return LineOutcome::Skip;
        }

        let Some(data) = line.strip_prefix("data:") else {
            // 忽略 event: / id: / retry: 等字段
            return LineOutcome::Skip;
        };
        let data = data.trim();
        if data.is_empty() {
            return LineOutcome::Skip;
        }
        if data == "[DONE]" {
            return LineOutcome::Done;
        }

        if let Some(hook) = &self.hook {
            hook(data);
        }

        match serde_json::from_str::<ChatChunk>(data) {
            Ok(chunk) => {
                // 流内错误包必须上抛，否则会被当成「空增量」静默吞掉，
                // 最终表现成一条成功的空回复
                if let Some(error) = &chunk.error {
                    return LineOutcome::Fatal(api_error_from_value(200, error, None));
                }
                LineOutcome::Events(chunk.into_events(&mut self.first_token_at))
            }
            Err(e) => LineOutcome::Fatal(LlmError::StreamParse(format!(
                "JSON 解析失败: {e}; 原始数据: {}",
                truncate(data, MAX_RAW_DATA_IN_ERROR)
            ))),
        }
    }
}

/// 截断字符串并补省略号（按字符边界切，避免破坏 UTF-8）
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str("...(truncated)");
    out
}

/// 流式响应中的单个 chunk（SSE data 字段反序列化结果）
#[derive(Debug, Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    /// 部分 API 在最后一个 chunk 中返回 usage
    #[serde(default)]
    usage: Option<Usage>,
    /// 部分 API（如 vLLM）在最后一个 chunk 中返回 timings
    #[serde(default)]
    timings: Option<Timings>,
    /// 流内错误（HTTP 仍是 200）
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    index: u32,
    /// 部分实现会在结束 chunk 中省略 delta，缺失时按空增量处理
    #[serde(default)]
    delta: DeltaMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

/// 流式增量消息
#[derive(Debug, Default, Deserialize)]
struct DeltaMessage {
    /// 正文增量
    #[serde(default)]
    content: Option<String>,
    /// 思考内容增量（推理模型在思考阶段会持续输出此字段）
    #[serde(default)]
    reasoning_content: Option<String>,
    /// 思考内容增量（部分网关使用此字段名）
    #[serde(default)]
    reasoning: Option<String>,
    /// 工具调用增量
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
}

impl ChatChunk {
    /// 将原始 chunk 转换为语义化事件
    ///
    /// 一个 chunk 可能同时携带思考、正文、工具调用与结束标记（例如推理刚结束的那一刻），
    /// 因此返回 `Vec`：调用方按顺序派发，任何一路内容都不会丢。
    fn into_events(self, first_token_at: &mut Option<Instant>) -> Vec<StreamEvent> {
        let mut events = Vec::with_capacity(3);
        let mut first_seen = false;

        for choice in &self.choices {
            let index = choice.index;
            let delta = &choice.delta;

            // 思考增量：`reasoning_content` 优先，缺失时回退到 `reasoning`
            let reasoning = delta
                .reasoning_content
                .as_deref()
                .filter(|s| !s.is_empty())
                .or_else(|| delta.reasoning.as_deref().filter(|s| !s.is_empty()));
            if let Some(text) = reasoning {
                first_seen = true;
                events.push(StreamEvent::ReasoningDelta {
                    index,
                    text: text.to_string(),
                });
            }

            // 正文增量（与思考增量互不冲突，可同时派发）
            if let Some(text) = delta.content.as_deref().filter(|s| !s.is_empty()) {
                first_seen = true;
                events.push(StreamEvent::ContentDelta {
                    index,
                    text: text.to_string(),
                });
            }

            // 工具调用增量
            if let Some(tool_calls) = &delta.tool_calls {
                for delta in tool_calls {
                    first_seen = true;
                    events.push(StreamEvent::ToolCallDelta {
                        index,
                        delta: delta.clone(),
                    });
                }
            }

            // 结束标记（可能携带 usage / timings；也可能只是空尾包）
            if let Some(reason) = &choice.finish_reason {
                events.push(StreamEvent::Finish(FinishInfo {
                    reason: reason.clone(),
                    index: Some(index),
                    usage: self.usage.clone(),
                    timings: self.timings.clone(),
                    first_token_at: *first_token_at,
                    done: false,
                }));
            }
        }

        // 在解析层打时间戳：比消费端（channel 出队、跨任务调度之后）更接近字节真实到达
        if first_seen && first_token_at.is_none() {
            *first_token_at = Some(Instant::now());
        }
        if first_seen {
            for event in &mut events {
                if let StreamEvent::Finish(info) = event {
                    info.first_token_at = *first_token_at;
                }
            }
        }

        // 某些 API 的最后一个 chunk 只包含 usage / timings 而 choices 为空。
        // 只要两者任一存在就必须产出 Finish 事件，否则这些数据会被丢弃。
        if events.is_empty() && (self.usage.is_some() || self.timings.is_some()) {
            events.push(StreamEvent::Finish(FinishInfo {
                reason: String::new(),
                index: None,
                usage: self.usage,
                timings: self.timings,
                first_token_at: *first_token_at,
                done: false,
            }));
        }

        events
    }
}

/// 解析一段 SSE 文本（测试与文档示例使用）
#[cfg(test)]
fn decode_all(text: &str) -> Vec<Result<StreamEvent>> {
    let mut decoder = SseDecoder::new(None);
    decoder.push(text.as_bytes());
    decoder.flush();
    let mut out = Vec::new();
    while let Some(event) = decoder.next_event() {
        out.push(event);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events_of(text: &str) -> Vec<StreamEvent> {
        decode_all(text).into_iter().map(|e| e.unwrap()).collect()
    }

    /// 服务端持续返回不含换行的数据时，行缓冲必须有上限，不能无界增长
    #[test]
    fn test_rejects_oversized_line() {
        let mut decoder = SseDecoder::new(None);
        for _ in 0..6 {
            decoder.push(&vec![b'a'; 1024 * 1024]);
        }
        let first = decoder.next_event();
        assert!(
            matches!(first, Some(Err(LlmError::StreamParse(_)))),
            "超过缓冲上限应返回 StreamParse 错误"
        );
    }

    /// 同一 chunk 中 reasoning_content 与 content 同时存在时，两条都必须派发
    #[test]
    fn test_reasoning_and_content_coexist() {
        let events = events_of(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"尾句思考\",\"content\":\"正文第一段\"}}]}\n\n",
        );
        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[0], StreamEvent::ReasoningDelta { text, .. } if text == "尾句思考")
        );
        assert!(
            matches!(&events[1], StreamEvent::ContentDelta { text, .. } if text == "正文第一段")
        );
    }

    /// `reasoning_content` 与 `reasoning` 同时出现时只派发一次，且优先权威字段
    #[test]
    fn test_reasoning_alias_dedup() {
        let events = events_of(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"权威字段\",\"reasoning\":\"兜底字段\"}}]}\n\n",
        );
        assert_eq!(events.len(), 1, "同一语义字段不应派发两次");
        assert!(
            matches!(&events[0], StreamEvent::ReasoningDelta { text, .. } if text == "权威字段")
        );
    }

    /// 只有 timings、没有 usage 的尾 chunk 也必须产出 Finish
    #[test]
    fn test_timings_only_chunk_emits_finish() {
        let events = events_of(
            "data: {\"choices\":[],\"timings\":{\"prompt_n\":4,\"prompt_ms\":100,\"prompt_per_second\":40,\"predicted_n\":42,\"predicted_ms\":1000,\"predicted_per_second\":42}}\n\n",
        );
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Finish(info) => {
                assert!(info.index.is_none());
                assert!(info.usage.is_none());
                assert_eq!(info.timings.as_ref().unwrap().predicted_per_second, 42.0);
            }
            other => panic!("预期 Finish，实际 {other:?}"),
        }
    }

    /// 结束 chunk 省略 delta 时不应报错
    #[test]
    fn test_chunk_without_delta() {
        let events =
            events_of("data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}\n\n");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Finish(info) if info.reason == "stop"));
    }

    /// 流内错误包必须变成错误，而不是被当成空增量吞掉
    #[test]
    fn test_error_chunk_is_fatal() {
        let results = decode_all(
            "data: {\"error\":{\"message\":\"overloaded\",\"code\":\"server_error\"}}\n\n",
        );
        assert_eq!(results.len(), 1);
        match &results[0] {
            Err(LlmError::Api { message, code, .. }) => {
                assert_eq!(message, "overloaded");
                assert_eq!(code.as_deref(), Some("server_error"));
            }
            other => panic!("流内错误必须上抛，实际: {other:?}"),
        }
    }

    /// 最后一帧没有尾随换行时，flush 之后不能丢
    #[test]
    fn test_trailing_line_without_newline() {
        let results = decode_all("data: {\"choices\":[{\"delta\":{\"content\":\"最后一帧\"}}]}");
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], Ok(StreamEvent::ContentDelta { text, .. }) if text == "最后一帧")
        );
    }

    /// `[DONE]` 必须补发结束标记，且其后再无事件
    #[test]
    fn test_done_marker() {
        let events =
            events_of("data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n");
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[1], StreamEvent::Finish(info) if info.done));
    }

    /// 多候选：`index` 必须原样带出，不能只取第一个 choice
    #[test]
    fn test_multiple_choices() {
        let events = events_of(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"A\"}},{\"index\":1,\"delta\":{\"content\":\"B\"}}]}\n\n",
        );
        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[0], StreamEvent::ContentDelta { index: 0, text, .. } if text == "A")
        );
        assert!(
            matches!(&events[1], StreamEvent::ContentDelta { index: 1, text, .. } if text == "B")
        );
    }

    /// 工具调用增量
    #[test]
    fn test_tool_call_delta() {
        let events = events_of(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{}\"}}]}}]}\n\n",
        );
        match &events[0] {
            StreamEvent::ToolCallDelta { index, delta } => {
                assert_eq!(*index, 0);
                assert_eq!(delta.id.as_deref(), Some("call_1"));
                assert_eq!(
                    delta.function.as_ref().unwrap().name.as_deref(),
                    Some("get_weather")
                );
            }
            other => panic!("预期 ToolCallDelta，实际 {other:?}"),
        }
    }

    /// 注释心跳行、event: 行、空行都要跳过
    #[test]
    fn test_skips_non_data_lines() {
        let events = events_of(": keep-alive\n\nevent: message\nid: 1\ndata: [DONE]\n\n");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Finish(info) if info.done));
    }

    /// 结束事件必须带解析层打点的首 token 时刻
    #[test]
    fn test_first_token_stamped() {
        let events = events_of(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"嗯\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\ndata: [DONE]\n\n",
        );
        let finish = events
            .iter()
            .find_map(|e| match e {
                StreamEvent::Finish(info) => Some(info),
                _ => None,
            })
            .expect("应有结束事件");
        assert!(finish.first_token_at.is_some(), "首 token 时刻应被打点");
    }
}
