//! 客户端实现：请求构造、重试、取消、流式适配

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{HeaderName, ACCEPT, CONTENT_TYPE, RETRY_AFTER};
use reqwest::{Method, RequestBuilder, Response};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::{AuthStyle, ClientConfig};
use crate::error::{api_error_from_value, LlmError, Result};
use crate::message::ChatMessage;
use crate::params::{ChatParams, EmbeddingParams};
use crate::response::{ChatCompletion, EmbeddingResponse, ModelInfo, ModelsResponse};
use crate::sse::SseDecoder;
use crate::stream::ChatStream;

/// 错误响应体最多保留的字节数
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;
/// 单次重试的最长等待
const MAX_RETRY_DELAY: Duration = Duration::from_secs(8);
/// 错误信息中保留的响应片段长度
const MAX_ERROR_TEXT_CHARS: usize = 512;

/// 单次请求的运行时选项
///
/// 与 [`ClientConfig`] 的区别：配置描述「客户端是什么」，选项描述「这一次请求怎么做」。
#[derive(Clone, Default)]
pub struct RequestOptions {
    /// 取消信号：取消后请求会尽快返回 [`LlmError::Canceled`]
    pub cancel: Option<CancellationToken>,
    /// 覆盖客户端的最大重试次数
    pub max_retries: Option<u32>,
    /// 覆盖响应体读取超时
    pub total_timeout: Option<Duration>,
}

impl RequestOptions {
    /// 创建默认选项
    pub fn new() -> Self {
        Self::default()
    }

    /// 绑定取消信号
    pub fn with_cancel(mut self, token: CancellationToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// 覆盖最大重试次数
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = Some(retries);
        self
    }

    /// 覆盖响应体读取超时
    pub fn with_total_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.total_timeout = timeout;
        self
    }
}

/// 大模型 API 客户端
///
/// 克隆开销极小：内部 `reqwest::Client` 共享连接池，仅做引用计数。
#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    config: ClientConfig,
}

impl LlmClient {
    /// 创建客户端实例
    pub fn new(config: ClientConfig) -> Result<Self> {
        if config.base_url().trim().is_empty() {
            return Err(LlmError::InvalidRequest("base_url 不能为空".to_string()));
        }
        let http = config.build_http()?;
        Ok(Self { http, config })
    }

    /// 客户端配置
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// 底层 HTTP 客户端（需要自定义请求时使用）
    pub fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    /// **非流式调用**：一次性等待模型生成完毕，返回完整结果
    pub async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        params: &ChatParams,
    ) -> Result<ChatCompletion> {
        self.chat_with(&messages, params, &RequestOptions::default())
            .await
    }

    /// 非流式调用（带运行时选项）
    pub async fn chat_with(
        &self,
        messages: &[ChatMessage],
        params: &ChatParams,
        options: &RequestOptions,
    ) -> Result<ChatCompletion> {
        let body = build_chat_body(messages, params, false)?;
        let url = self.config.chat_url();
        let response = self.send(Method::POST, &url, Some(&body), options).await?;
        let timeout = options
            .total_timeout
            .or_else(|| self.config.total_timeout());
        let bytes = self.read_body(response, timeout).await?;

        let completion: ChatCompletion = serde_json::from_slice(&bytes)?;
        // 部分服务在 HTTP 200 时把错误放进响应体
        if let Some(error) = &completion.error {
            return Err(api_error_from_value(200, error, None));
        }
        Ok(completion)
    }

    /// **流式调用**：通过 SSE 逐 token 接收模型输出
    ///
    /// 返回的 [`ChatStream`] 实现了 `Stream`，也可以直接用 [`ChatStream::recv`]。
    /// 丢弃它会立即关闭底层连接。
    pub async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        params: &ChatParams,
    ) -> Result<ChatStream> {
        self.chat_stream_with(&messages, params, RequestOptions::default())
            .await
    }

    /// 流式调用（带运行时选项）
    pub async fn chat_stream_with(
        &self,
        messages: &[ChatMessage],
        params: &ChatParams,
        options: RequestOptions,
    ) -> Result<ChatStream> {
        let url = self.config.chat_url();
        let body = build_chat_body(messages, params, true)?;
        let response = match self.send(Method::POST, &url, Some(&body), &options).await {
            Ok(response) => response,
            // 部分老版本网关不认识 stream_options 会直接 400/422，
            // 去掉该字段再试一次，换取更广的兼容性（代价是拿不到 usage）
            Err(error) if is_stream_options_error(&error) => {
                let fallback = ChatParams {
                    stream_options: crate::params::StreamOptions {
                        include_usage: false,
                    },
                    ..params.clone()
                };
                let body = build_chat_body(messages, &fallback, true)?;
                self.send(Method::POST, &url, Some(&body), &options).await?
            }
            Err(error) => return Err(error),
        };

        // 个别网关会把错误以 200 + JSON 返回；先看 Content-Type 拦下来，
        // 否则整段 JSON 都会被当作「非 data 行」跳过，最终表现成空回复。
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !content_type.is_empty() && !content_type.contains("event-stream") {
            let text = read_text_limited(response, MAX_ERROR_BODY_BYTES).await;
            return Err(match serde_json::from_str::<Value>(&text) {
                Ok(value) => api_error_from_value(200, &value, None),
                Err(_) => LlmError::StreamParse(format!(
                    "响应不是 SSE（Content-Type: {content_type}）: {}",
                    truncate(&text, MAX_ERROR_TEXT_CHARS)
                )),
            });
        }

        let state = StreamState {
            bytes: Box::pin(response.bytes_stream()),
            decoder: SseDecoder::new(self.config.debug_hook().cloned()),
            idle_timeout: self.config.stream_idle_timeout(),
            cancel: options.cancel.clone(),
            finished: false,
        };

        let inner = futures_util::stream::unfold(state, |mut state| async move {
            loop {
                // 1) 缓冲区里已解析的事件优先派发
                if let Some(event) = state.decoder.next_event() {
                    return Some((event, state));
                }
                if state.finished || state.decoder.is_done() {
                    return None;
                }

                // 2) 取消：立刻结束，上层据此标记 canceled
                if state
                    .cancel
                    .as_ref()
                    .is_some_and(|token| token.is_cancelled())
                {
                    state.finished = true;
                    return Some((Err(LlmError::Canceled), state));
                }

                // 3) 读取下一块数据：空闲超时 + 取消信号都能打断等待
                let outcome = {
                    let read = state.bytes.next();
                    match state.idle_timeout {
                        Some(timeout) => tokio::select! {
                            biased;
                            _ = cancel_wait(state.cancel.as_ref()) => ReadOutcome::Canceled,
                            result = tokio::time::timeout(timeout, read) => match result {
                                Ok(next) => ReadOutcome::Next(next),
                                Err(_) => ReadOutcome::Idle(timeout),
                            },
                        },
                        None => tokio::select! {
                            biased;
                            _ = cancel_wait(state.cancel.as_ref()) => ReadOutcome::Canceled,
                            next = read => ReadOutcome::Next(next),
                        },
                    }
                };

                match outcome {
                    ReadOutcome::Next(Some(Ok(chunk))) => state.decoder.push(&chunk),
                    ReadOutcome::Next(Some(Err(err))) => {
                        state.finished = true;
                        return Some((Err(LlmError::Http(err)), state));
                    }
                    ReadOutcome::Next(None) => {
                        // 读到 EOF：把「没有尾随换行」的最后一行也解析掉
                        state.finished = true;
                        state.decoder.flush();
                    }
                    ReadOutcome::Canceled => {
                        state.finished = true;
                        return Some((Err(LlmError::Canceled), state));
                    }
                    ReadOutcome::Idle(timeout) => {
                        state.finished = true;
                        return Some((
                            Err(LlmError::IdleTimeout {
                                idle_ms: timeout.as_millis() as u64,
                            }),
                            state,
                        ));
                    }
                }
            }
        });

        Ok(ChatStream::new(Box::pin(inner)))
    }

    /// 列出可用模型（`GET /models`）
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let url = self.config.endpoint_url("models");
        let response = self
            .send(Method::GET, &url, None, &RequestOptions::default())
            .await?;
        let bytes = self
            .read_body(response, self.config.total_timeout())
            .await?;
        let models: ModelsResponse = serde_json::from_slice(&bytes)?;
        Ok(models.data)
    }

    /// 文本向量化（`POST /embeddings`）
    pub async fn embeddings(&self, params: &EmbeddingParams) -> Result<EmbeddingResponse> {
        let body = serde_json::to_value(params)?;
        let url = self.config.endpoint_url("embeddings");
        let response = self
            .send(Method::POST, &url, Some(&body), &RequestOptions::default())
            .await?;
        let bytes = self
            .read_body(response, self.config.total_timeout())
            .await?;

        let value: Value = serde_json::from_slice(&bytes)?;
        if let Some(error) = value.get("error") {
            return Err(api_error_from_value(200, error, None));
        }
        Ok(serde_json::from_value(value)?)
    }

    /// 通用逃生口：向任意路径 POST JSON 并反序列化响应
    ///
    /// 用于库尚未封装的接口（新的 `/responses`、厂商私有接口等），
    /// 复用同一套认证、重试、超时与错误解析。
    pub async fn post_json<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        let url = if path.starts_with("http") {
            path.to_string()
        } else {
            self.config.endpoint_url(path)
        };
        let response = self
            .send(Method::POST, &url, Some(body), &RequestOptions::default())
            .await?;
        let bytes = self
            .read_body(response, self.config.total_timeout())
            .await?;
        let value: Value = serde_json::from_slice(&bytes)?;
        if let Some(error) = value.get("error") {
            return Err(api_error_from_value(200, error, None));
        }
        Ok(serde_json::from_value(value)?)
    }

    /// 发送请求（含重试与取消），返回 2xx 响应
    async fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
        options: &RequestOptions,
    ) -> Result<Response> {
        let max_retries = options
            .max_retries
            .unwrap_or_else(|| self.config.max_retries());
        let mut attempt: u32 = 0;

        loop {
            let request = self.build_request(method.clone(), url, body)?;
            let response_timeout = self.config.response_timeout();

            // 只对「等待响应头」计时：body 读取另有空闲/总超时，
            // 避免长流被总超时误杀。
            let send = async move {
                match response_timeout {
                    Some(timeout) => match tokio::time::timeout(timeout, request.send()).await {
                        Ok(result) => result.map_err(LlmError::Http),
                        Err(_) => Err(LlmError::Timeout(format!(
                            "{}s 内未收到响应",
                            timeout.as_secs()
                        ))),
                    },
                    None => request.send().await.map_err(LlmError::Http),
                }
            };

            let result = match &options.cancel {
                Some(token) => tokio::select! {
                    biased;
                    _ = token.cancelled() => Err(LlmError::Canceled),
                    result = send => result,
                },
                None => send.await,
            };

            let error = match result {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => self.error_from_response(response).await,
                Err(error) => error,
            };

            if attempt < max_retries && error.is_retryable() {
                attempt += 1;
                let delay = self.retry_delay(attempt, error.retry_after());
                sleep_or_cancel(delay, options.cancel.as_ref()).await?;
                continue;
            }
            return Err(error);
        }
    }

    /// 构造请求：认证头、Accept、JSON 体
    fn build_request(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<RequestBuilder> {
        let mut request = self
            .http
            .request(method, url)
            // 显式声明可接受 SSE：部分反向代理缺少该头时会缓冲整个流
            .header(ACCEPT, "application/json, text/event-stream");
        request = self.apply_auth(request)?;
        if let Some(body) = body {
            request = request.json(body);
        }
        Ok(request)
    }

    /// 按配置的认证方式附加凭据
    fn apply_auth(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        let key = self.config.api_key();
        if key.is_empty() {
            return Ok(request);
        }
        Ok(match self.config.auth() {
            AuthStyle::Bearer => request.bearer_auth(key),
            AuthStyle::Header(name) => {
                let header = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|e| LlmError::InvalidRequest(format!("非法认证头名 {name}: {e}")))?;
                request.header(header, key)
            }
            // 查询参数形态在拼接 URL 时已处理
            AuthStyle::Query(_) | AuthStyle::None => request,
        })
    }

    /// 解析错误响应
    async fn error_from_response(&self, response: Response) -> LlmError {
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after);

        let text = read_text_limited(response, MAX_ERROR_BODY_BYTES).await;
        match serde_json::from_str::<Value>(&text) {
            Ok(value) => api_error_from_value(status, &value, retry_after),
            Err(_) => LlmError::Api {
                status,
                message: truncate(text.trim(), MAX_ERROR_TEXT_CHARS),
                code: None,
                error_type: None,
                param: None,
                retry_after_ms: retry_after.map(|d| d.as_millis() as u64),
            },
        }
    }

    /// 读取成功响应的响应体（带大小上限与超时）
    async fn read_body(&self, response: Response, timeout: Option<Duration>) -> Result<Vec<u8>> {
        let limit = self.config.max_response_bytes();
        let read = read_bytes_limited(response, limit);
        match timeout {
            Some(timeout) => match tokio::time::timeout(timeout, read).await {
                Ok(result) => result,
                Err(_) => Err(LlmError::Timeout(format!(
                    "{}s 内未读完响应体",
                    timeout.as_secs()
                ))),
            },
            None => read.await,
        }
    }

    /// 指数退避（优先采用服务端建议的等待时间）
    fn retry_delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(after) = retry_after {
            return after.min(MAX_RETRY_DELAY);
        }
        let shift = attempt.saturating_sub(1).min(5);
        self.config
            .retry_base_delay()
            .saturating_mul(1u32 << shift)
            .min(MAX_RETRY_DELAY)
    }
}

/// 流式读取状态
struct StreamState {
    bytes:
        std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    decoder: SseDecoder,
    idle_timeout: Option<Duration>,
    cancel: Option<CancellationToken>,
    finished: bool,
}

/// 单次读取的结果
enum ReadOutcome {
    /// 读到一块数据；`None` 表示流结束
    Next(Option<reqwest::Result<bytes::Bytes>>),
    /// 被调用方取消
    Canceled,
    /// 空闲超时
    Idle(Duration),
}

/// 等待取消信号；未配置时永不完成（用于 `select!` 占位）
async fn cancel_wait(token: Option<&CancellationToken>) {
    match token {
        Some(token) => token.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

/// 构造 chat completions 请求体
///
/// `messages` / `stream` 由库统一写入，`extra` 里的同名字段会被覆盖，
/// 保证调用方无法意外发出自相矛盾的请求。
pub(crate) fn build_chat_body(
    messages: &[ChatMessage],
    params: &ChatParams,
    stream: bool,
) -> Result<Value> {
    if params.model.trim().is_empty() {
        return Err(LlmError::InvalidRequest("model 不能为空".to_string()));
    }
    if messages.is_empty() {
        return Err(LlmError::InvalidRequest("messages 不能为空".to_string()));
    }

    let mut body = serde_json::to_value(params)?;
    let Some(object) = body.as_object_mut() else {
        return Err(LlmError::InvalidRequest(
            "请求体必须是 JSON 对象".to_string(),
        ));
    };

    object.insert("messages".to_string(), serde_json::to_value(messages)?);
    object.insert("stream".to_string(), Value::Bool(stream));

    if stream {
        if params.stream_options.include_usage {
            object
                .entry("stream_options")
                .or_insert_with(|| serde_json::json!({ "include_usage": true }));
        }
    } else {
        // 非流式请求不应该带流式字段，严格的服务端会直接拒绝
        object.remove("stream_options");
    }

    Ok(body)
}

/// 解析 `Retry-After`（只支持秒数形式）
fn parse_retry_after(raw: &str) -> Option<Duration> {
    raw.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// 判断错误是否是「不支持 stream_options」的请求被拒
fn is_stream_options_error(error: &LlmError) -> bool {
    matches!(
        error,
        LlmError::Api {
            status: 400 | 422,
            message,
            ..
        } if message.contains("stream_options")
    )
}

/// 读取响应体，超过上限立刻中断
async fn read_bytes_limited(response: Response, limit: usize) -> Result<Vec<u8>> {
    if let Some(length) = response.content_length() {
        if length as usize > limit {
            return Err(LlmError::ResponseTooLarge { limit });
        }
    }

    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if buffer.len() + chunk.len() > limit {
            return Err(LlmError::ResponseTooLarge { limit });
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(buffer)
}

/// 读取文本（用于错误信息，超过上限即停止）
async fn read_text_limited(response: Response, limit: usize) -> String {
    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { break };
        let remaining = limit.saturating_sub(buffer.len());
        if remaining == 0 {
            break;
        }
        let take = remaining.min(chunk.len());
        buffer.extend_from_slice(&chunk[..take]);
        if buffer.len() >= limit {
            break;
        }
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

/// 截断文本（按字符边界，避免破坏 UTF-8）
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str("...(truncated)");
    out
}

/// 可被取消打断的 sleep
async fn sleep_or_cancel(delay: Duration, cancel: Option<&CancellationToken>) -> Result<()> {
    if delay.is_zero() {
        return Ok(());
    }
    match cancel {
        Some(token) => tokio::select! {
            biased;
            _ = token.cancelled() => Err(LlmError::Canceled),
            _ = tokio::time::sleep(delay) => Ok(()),
        },
        None => {
            tokio::time::sleep(delay).await;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ChatParams;

    fn messages() -> Vec<ChatMessage> {
        vec![ChatMessage::user("hi")]
    }

    /// 请求体：messages / stream / stream_options 由库统一写入
    #[test]
    fn test_build_stream_body() {
        let params = ChatParams::new("test-model");
        let body = build_chat_body(&messages(), &params, true).unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    /// 关闭 usage 时不应写入 stream_options
    #[test]
    fn test_build_stream_body_without_usage() {
        let params = ChatParams::new("m").without_stream_usage();
        let body = build_chat_body(&messages(), &params, true).unwrap();
        assert!(body.get("stream_options").is_none());
    }

    /// 非流式请求必须剔除流式字段（即使 extra 里塞了）
    #[test]
    fn test_build_non_stream_body_removes_stream_options() {
        let params = ChatParams::new("m")
            .with_extra("stream_options", serde_json::json!({"include_usage": true}));
        let body = build_chat_body(&messages(), &params, false).unwrap();
        assert_eq!(body["stream"], false);
        assert!(body.get("stream_options").is_none());
    }

    /// model / messages 的非法组合要在发送前拦住
    #[test]
    fn test_build_body_validates_input() {
        let params = ChatParams::new("   ");
        assert!(matches!(
            build_chat_body(&messages(), &params, false),
            Err(LlmError::InvalidRequest(_))
        ));

        let params = ChatParams::new("m");
        assert!(matches!(
            build_chat_body(&[], &params, false),
            Err(LlmError::InvalidRequest(_))
        ));
    }

    /// Retry-After 只认秒数
    #[test]
    fn test_parse_retry_after() {
        assert_eq!(parse_retry_after("3"), Some(Duration::from_secs(3)));
        assert_eq!(parse_retry_after(" 12 "), Some(Duration::from_secs(12)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    }

    /// 退避时间：指数增长且有上限，Retry-After 优先
    #[test]
    fn test_retry_delay() {
        let client = LlmClient::new(ClientConfig::new("https://x/v1", "k")).unwrap();
        assert_eq!(client.retry_delay(1, None), Duration::from_millis(500));
        assert_eq!(client.retry_delay(2, None), Duration::from_millis(1000));
        assert_eq!(client.retry_delay(3, None), Duration::from_millis(2000));
        assert_eq!(client.retry_delay(9, None), MAX_RETRY_DELAY);
        assert_eq!(
            client.retry_delay(1, Some(Duration::from_secs(2))),
            Duration::from_secs(2)
        );
        // 服务端要求的等待时间也不能无限长
        assert_eq!(
            client.retry_delay(1, Some(Duration::from_secs(600))),
            MAX_RETRY_DELAY
        );
    }

    /// 空 base_url 直接拒绝
    #[test]
    fn test_empty_base_url_rejected() {
        assert!(matches!(
            LlmClient::new(ClientConfig::new("  ", "k")),
            Err(LlmError::InvalidRequest(_))
        ));
    }
}
