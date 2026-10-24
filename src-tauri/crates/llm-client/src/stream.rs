//! 流式事件、结果累加器与流对象

use std::collections::BTreeMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::message::{FunctionCall, ToolCall};
use crate::response::{Timings, Usage};

/// 流式事件
///
/// 一个 SSE 分片可能同时包含思考、正文与工具调用增量，因此这里把各类增量拆成独立事件，
/// 由调用方按顺序处理（也可以直接丢给 [`StreamAccumulator`]）。
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum StreamEvent {
    /// 思考/推理内容的增量文本
    ReasoningDelta {
        /// 候选序号（`n > 1` 时用于区分）
        index: u32,
        /// 增量文本
        text: String,
    },
    /// 正文回复的增量文本
    ContentDelta {
        /// 候选序号
        index: u32,
        /// 增量文本
        text: String,
    },
    /// 工具调用增量（`id` / `name` 只在首个分片出现，`arguments` 逐片拼接）
    ToolCallDelta {
        /// 候选序号
        index: u32,
        /// 增量内容
        delta: ToolCallDelta,
    },
    /// 生成结束（可能因 usage / timings / `[DONE]` 分别到达而出现多次，需按字段合并）
    Finish(FinishInfo),
}

impl StreamEvent {
    /// 候选序号（Finish 事件在没有绑定候选时返回 `None`）
    pub fn index(&self) -> Option<u32> {
        match self {
            Self::ReasoningDelta { index, .. }
            | Self::ContentDelta { index, .. }
            | Self::ToolCallDelta { index, .. } => Some(*index),
            Self::Finish(info) => info.index,
        }
    }

    /// 文本增量（非文本事件返回 `None`）
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::ReasoningDelta { text, .. } | Self::ContentDelta { text, .. } => Some(text),
            _ => None,
        }
    }

    /// 是否为结束事件
    pub fn is_finish(&self) -> bool {
        matches!(self, Self::Finish(_))
    }
}

/// 工具调用增量
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCallDelta {
    /// 第几个工具调用（同一轮可能有多个）
    #[serde(default)]
    pub index: u32,
    /// 调用 ID（仅首个分片携带）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// 类型（仅首个分片携带）
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// 函数信息增量
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<FunctionCallDelta>,
}

/// 函数信息增量
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FunctionCallDelta {
    /// 函数名（仅首个分片携带）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 参数片段（逐片拼接成完整 JSON 字符串）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// 流式结束信息
#[derive(Debug, Clone, Default)]
pub struct FinishInfo {
    /// 结束原因（`[DONE]` 与仅含 usage 的尾包为空字符串）
    pub reason: String,
    /// 绑定的候选序号（仅含 usage 的尾包为 `None`）
    pub index: Option<u32>,
    /// Token 使用量（需要 API 支持 `stream_options.include_usage`）
    pub usage: Option<Usage>,
    /// Token 速度统计（vLLM 等 API 在最后一个 chunk 提供）
    pub timings: Option<Timings>,
    /// 首个增量（思考或正文）到达时刻，由 SSE 解析层在字节到达时打点
    ///
    /// 不能取「首个正文」时刻：推理模型会先长时间输出思考内容，
    /// 用正文时刻会把思考生成时间误算进 prefill。
    pub first_token_at: Option<Instant>,
    /// 是否为 `[DONE]` 标记产生的结束事件
    pub done: bool,
}

impl FinishInfo {
    /// 构造 `[DONE]` 结束事件
    pub(crate) fn done(first_token_at: Option<Instant>) -> Self {
        Self {
            done: true,
            first_token_at,
            ..Self::default()
        }
    }

    /// 按字段合并另一次结束信息
    ///
    /// 不同实现会把 usage、timings、finish_reason 拆到不同 chunk，
    /// 整体覆盖会丢掉先到的数据，因此只填补尚未拿到的字段。
    pub fn merge(&mut self, other: FinishInfo) {
        if self.reason.is_empty() {
            self.reason = other.reason;
        }
        if self.index.is_none() {
            self.index = other.index;
        }
        if self.usage.is_none() {
            self.usage = other.usage;
        }
        if self.timings.is_none() {
            self.timings = other.timings;
        }
        if other.first_token_at.is_some() {
            self.first_token_at = other.first_token_at;
        }
        self.done |= other.done;
    }
}

/// 工具调用累积草稿（流式下各字段是分段到达的）
#[derive(Debug, Clone, Default)]
struct ToolCallDraft {
    index: u32,
    id: String,
    kind: String,
    name: String,
    arguments: String,
}

impl ToolCallDraft {
    fn merge(&mut self, delta: &ToolCallDelta) {
        self.index = delta.index;
        if let Some(id) = delta.id.as_deref().filter(|s| !s.is_empty()) {
            self.id = id.to_string();
        }
        if let Some(kind) = delta.kind.as_deref().filter(|s| !s.is_empty()) {
            self.kind = kind.to_string();
        }
        if let Some(function) = &delta.function {
            if let Some(name) = function.name.as_deref().filter(|s| !s.is_empty()) {
                self.name = name.to_string();
            }
            if let Some(arguments) = &function.arguments {
                self.arguments.push_str(arguments);
            }
        }
    }

    fn into_tool_call(self) -> ToolCall {
        ToolCall {
            id: self.id,
            kind: if self.kind.is_empty() {
                "function".to_string()
            } else {
                self.kind
            },
            function: FunctionCall {
                name: self.name,
                arguments: self.arguments,
            },
        }
    }
}

/// 单个候选的累积结果
#[derive(Debug, Clone, Default)]
pub struct ChoiceAccumulator {
    index: u32,
    content: String,
    reasoning_content: String,
    finish_reason: Option<String>,
    tool_calls: Vec<ToolCallDraft>,
}

impl ChoiceAccumulator {
    /// 候选序号
    pub fn index(&self) -> u32 {
        self.index
    }

    /// 累积的正文
    pub fn content(&self) -> &str {
        &self.content
    }

    /// 累积的思考内容
    pub fn reasoning_content(&self) -> &str {
        &self.reasoning_content
    }

    /// 结束原因
    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }

    /// 累积的工具调用
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.tool_calls
            .iter()
            .cloned()
            .map(ToolCallDraft::into_tool_call)
            .collect()
    }
}

/// 流式结果聚合器
///
/// 把零散事件累积成完整结果，并按字段合并多次到达的结束信息。
/// 支持 `n > 1` 的多候选：每个候选各自累积，另提供访问序号最小候选的便捷方法。
#[derive(Debug, Clone, Default)]
pub struct StreamAccumulator {
    choices: BTreeMap<u32, ChoiceAccumulator>,
    /// Token 使用量（合并首个非空值）
    pub usage: Option<Usage>,
    /// Token 速度统计（合并首个非空值）
    pub timings: Option<Timings>,
    /// 首个增量到达时刻（优先采用解析层打点）
    pub first_token_at: Option<Instant>,
    /// 是否收到过带 `finish_reason` 的结束事件
    pub saw_finish: bool,
    /// 是否收到过 `[DONE]` 标记
    pub saw_done: bool,
}

impl StreamAccumulator {
    /// 喂入一个流式事件
    pub fn feed(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::ReasoningDelta { index, text } => {
                self.mark_first_token(None);
                self.choice_mut(*index).reasoning_content.push_str(text);
            }
            StreamEvent::ContentDelta { index, text } => {
                self.mark_first_token(None);
                self.choice_mut(*index).content.push_str(text);
            }
            StreamEvent::ToolCallDelta { index, delta } => {
                self.mark_first_token(None);
                let choice = self.choice_mut(*index);
                let slot = delta.index as usize;
                while choice.tool_calls.len() <= slot {
                    choice.tool_calls.push(ToolCallDraft::default());
                }
                choice.tool_calls[slot].merge(delta);
            }
            StreamEvent::Finish(info) => {
                self.mark_first_token(info.first_token_at);
                if let Some(index) = info.index {
                    if !info.reason.is_empty() {
                        self.choice_mut(index).finish_reason = Some(info.reason.clone());
                    }
                }
                if self.usage.is_none() {
                    self.usage = info.usage.clone();
                }
                if self.timings.is_none() {
                    self.timings = info.timings.clone();
                }
                self.saw_finish |= !info.reason.is_empty();
                self.saw_done |= info.done;
            }
        }
    }

    /// 首个候选（序号最小）的累积结果
    pub fn first_choice(&self) -> Option<&ChoiceAccumulator> {
        self.choices.values().next()
    }

    /// 指定候选的累积结果
    pub fn choice(&self, index: u32) -> Option<&ChoiceAccumulator> {
        self.choices.get(&index)
    }

    /// 所有候选
    pub fn choices(&self) -> impl Iterator<Item = &ChoiceAccumulator> {
        self.choices.values()
    }

    /// 第一个候选的正文
    pub fn content(&self) -> &str {
        self.first_choice().map(|c| c.content()).unwrap_or("")
    }

    /// 第一个候选的思考内容
    pub fn reasoning_content(&self) -> &str {
        self.first_choice()
            .map(|c| c.reasoning_content())
            .unwrap_or("")
    }

    /// 第一个候选的工具调用
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.first_choice()
            .map(ChoiceAccumulator::tool_calls)
            .unwrap_or_default()
    }

    /// 第一个候选的结束原因
    pub fn finish_reason(&self) -> Option<&str> {
        self.first_choice().and_then(|c| c.finish_reason())
    }

    /// 流是否正常走到结束（收到 `finish_reason` 或 `[DONE]`）
    ///
    /// 两者都没有说明连接被中途截断，此时正文大概率不完整。
    pub fn is_complete(&self) -> bool {
        self.saw_finish || self.saw_done
    }

    /// 取指定候选，不存在时创建
    fn choice_mut(&mut self, index: u32) -> &mut ChoiceAccumulator {
        self.choices
            .entry(index)
            .or_insert_with(|| ChoiceAccumulator {
                index,
                ..Default::default()
            })
    }

    /// 记录首个增量时刻
    ///
    /// 解析层打点的 `first_token_at` 比消费端更接近字节真实到达，因此优先采用；
    /// 但只能在拿到时覆盖，避免先记录的消费端时刻把权威值挤掉后又被置空。
    fn mark_first_token(&mut self, at: Option<Instant>) {
        match at {
            Some(at) => self.first_token_at = Some(at),
            None => {
                if self.first_token_at.is_none() {
                    self.first_token_at = Some(Instant::now());
                }
            }
        }
    }
}

/// 流式响应对象
///
/// 实现了 [`Stream`]，可以直接 `while let Some(event) = stream.next().await`，
/// 也可以用 [`ChatStream::recv`]。**丢弃该对象会立即关闭底层连接**，
/// 因此「取消」只需要把流 drop 掉，或通过 `RequestOptions::cancel` 显式触发。
pub struct ChatStream {
    inner: Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>,
}

impl ChatStream {
    /// 由内部流构造（仅在库内使用）
    pub(crate) fn new(inner: Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>) -> Self {
        Self { inner }
    }

    /// 取下一个事件，返回 `None` 表示流结束
    pub async fn recv(&mut self) -> Option<Result<StreamEvent>> {
        self.inner.next().await
    }

    /// 把流消费完并聚合成完整结果
    ///
    /// 中途出现错误时立即返回该错误（已累积的部分可以从 `Err` 之外的结果里看不到，
    /// 需要部分结果时请手动 `recv` 并喂给 [`StreamAccumulator`]）。
    pub async fn drain(mut self) -> Result<StreamAccumulator> {
        let mut accumulator = StreamAccumulator::default();
        while let Some(event) = self.recv().await {
            accumulator.feed(&event?);
        }
        Ok(accumulator)
    }
}

impl std::fmt::Debug for ChatStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatStream").finish_non_exhaustive()
    }
}

impl Stream for ChatStream {
    type Item = Result<StreamEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 正文与思考分别累积，多次 Finish 按字段合并（usage 与 timings 都不能丢）
    #[test]
    fn test_accumulator_merges_finish_fields() {
        let mut acc = StreamAccumulator::default();
        acc.feed(&StreamEvent::ReasoningDelta {
            index: 0,
            text: "思考".into(),
        });
        acc.feed(&StreamEvent::ContentDelta {
            index: 0,
            text: "你好".into(),
        });
        acc.feed(&StreamEvent::Finish(FinishInfo {
            reason: "stop".into(),
            index: Some(0),
            usage: None,
            timings: Some(Timings {
                predicted_n: 10,
                predicted_ms: 500.0,
                ..Default::default()
            }),
            first_token_at: Some(Instant::now()),
            done: false,
        }));
        acc.feed(&StreamEvent::Finish(FinishInfo {
            reason: String::new(),
            usage: Some(Usage {
                prompt_tokens: 6,
                completion_tokens: 10,
                total_tokens: 16,
                ..Default::default()
            }),
            ..Default::default()
        }));
        acc.feed(&StreamEvent::Finish(FinishInfo::done(None)));

        assert_eq!(acc.content(), "你好");
        assert_eq!(acc.reasoning_content(), "思考");
        assert_eq!(acc.finish_reason(), Some("stop"));
        assert!(acc.usage.is_some(), "usage 不应被后续事件覆盖丢失");
        assert!(acc.timings.is_some(), "timings 不应被后续事件覆盖丢失");
        assert!(acc.saw_finish && acc.saw_done);
        assert!(acc.is_complete());
    }

    /// 多候选：同一个分片里不同 index 的内容不能串台
    #[test]
    fn test_accumulator_keeps_choices_separate() {
        let mut acc = StreamAccumulator::default();
        acc.feed(&StreamEvent::ContentDelta {
            index: 0,
            text: "候选0".into(),
        });
        acc.feed(&StreamEvent::ContentDelta {
            index: 1,
            text: "候选1".into(),
        });
        assert_eq!(acc.content(), "候选0");
        assert_eq!(acc.choice(1).unwrap().content(), "候选1");
    }

    /// 工具调用增量：id/name 只来一次，arguments 逐片拼接
    #[test]
    fn test_accumulator_merges_tool_call_deltas() {
        let mut acc = StreamAccumulator::default();
        acc.feed(&StreamEvent::ToolCallDelta {
            index: 0,
            delta: ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                kind: Some("function".into()),
                function: Some(FunctionCallDelta {
                    name: Some("get_weather".into()),
                    arguments: Some("{\"city\":".into()),
                }),
            },
        });
        acc.feed(&StreamEvent::ToolCallDelta {
            index: 0,
            delta: ToolCallDelta {
                index: 0,
                function: Some(FunctionCallDelta {
                    arguments: Some("\"北京\"}".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        });

        let calls = acc.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "get_weather");
        assert_eq!(calls[0].function.arguments, "{\"city\":\"北京\"}");
    }
}
