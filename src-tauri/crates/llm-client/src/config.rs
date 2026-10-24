//! 客户端配置
//!
//! [`ClientConfig`] 用「构造 + 链式 with_*」的方式配置，字段全部私有：
//! 后续新增配置项不会成为破坏性变更。

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use crate::error::{LlmError, Result};

/// 默认 API 地址（OpenAI 官方）
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// 默认连接超时
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 默认「等待响应头」超时（覆盖 prefill；推理模型首字前可能需要很久）
pub const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(600);
/// 默认流式空闲超时（两次数据到达之间的最大间隔）
pub const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// 默认非流式响应体读取超时
pub const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(600);
/// 默认单次响应体上限（32 MiB）
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
/// 默认重试退避基值
pub const DEFAULT_RETRY_BASE_DELAY: Duration = Duration::from_millis(500);

/// 认证方式
///
/// 不同厂商的鉴权差异很大，这里把三种主流形态做成可选项：
/// - OpenAI / DeepSeek / 通义：`Authorization: Bearer <key>`
/// - Azure OpenAI：`api-key: <key>`
/// - 部分自建网关：`?key=<key>`
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>`
    Bearer,
    /// 自定义请求头承载密钥，例如 `api-key`
    Header(String),
    /// 查询参数承载密钥，例如 `key`
    Query(String),
    /// 不发送认证信息（本地 / 内网服务）
    None,
}

/// 原始 SSE 数据回调（调试用）
///
/// 每个 `data:` 负载都会回调一次，便于排查「某厂商返回了什么奇怪的东西」。
/// 注意：回调在异步任务中同步执行，不要做重活；内容可能包含模型输出，注意日志脱敏。
pub type DebugHook = Arc<dyn Fn(&str) + Send + Sync>;

/// API 客户端配置
#[derive(Clone)]
#[non_exhaustive]
pub struct ClientConfig {
    base_url: String,
    api_key: String,
    auth: AuthStyle,
    chat_endpoint: Option<String>,
    connect_timeout: Option<Duration>,
    response_timeout: Option<Duration>,
    stream_idle_timeout: Option<Duration>,
    total_timeout: Option<Duration>,
    extra_headers: HeaderMap,
    user_agent: Option<String>,
    proxy: Option<String>,
    accept_invalid_certs: bool,
    max_retries: u32,
    retry_base_delay: Duration,
    max_response_bytes: usize,
    debug_hook: Option<DebugHook>,
    http: Option<reqwest::Client>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: String::new(),
            auth: AuthStyle::Bearer,
            chat_endpoint: None,
            connect_timeout: Some(DEFAULT_CONNECT_TIMEOUT),
            response_timeout: Some(DEFAULT_RESPONSE_TIMEOUT),
            stream_idle_timeout: Some(DEFAULT_STREAM_IDLE_TIMEOUT),
            total_timeout: Some(DEFAULT_TOTAL_TIMEOUT),
            extra_headers: HeaderMap::new(),
            user_agent: Some(concat!("llm-client/", env!("CARGO_PKG_VERSION")).to_string()),
            proxy: None,
            accept_invalid_certs: false,
            // 默认不重试：压测场景下重试会污染延迟统计，需要时显式开启
            max_retries: 0,
            retry_base_delay: DEFAULT_RETRY_BASE_DELAY,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            debug_hook: None,
            http: None,
        }
    }
}

impl ClientConfig {
    /// 用地址与密钥创建配置
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            ..Self::default()
        }
    }

    /// 创建不带头部认证的配置（本地服务，密钥通过 [`Self::with_auth`] 指定）
    pub fn anonymous(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            auth: AuthStyle::None,
            ..Self::default()
        }
    }

    /// 设置 API 密钥
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = api_key.into();
        self
    }

    /// 设置认证方式
    pub fn with_auth(mut self, auth: AuthStyle) -> Self {
        self.auth = auth;
        self
    }

    /// 设置基础地址（例如 `https://api.deepseek.com/v1`）
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 直接指定 chat 完整地址（用于 Azure 这类路径形态特殊的服务）
    pub fn with_chat_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.chat_endpoint = Some(endpoint.into());
        self
    }

    /// 连接（TCP + TLS 握手）超时
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// 等待响应头的超时（流式下即 prefill 上限）
    pub fn with_response_timeout(mut self, timeout: Duration) -> Self {
        self.response_timeout = Some(timeout);
        self
    }

    /// 流式读取的空闲超时（相邻两次数据之间的最大间隔），传 `None` 表示不限制
    pub fn with_stream_idle_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.stream_idle_timeout = timeout;
        self
    }

    /// 非流式响应体的读取超时，传 `None` 表示不限制
    pub fn with_total_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.total_timeout = timeout;
        self
    }

    /// 追加一个默认请求头（如 OpenRouter 的 `HTTP-Referer`）
    pub fn with_header(mut self, name: impl AsRef<str>, value: impl AsRef<str>) -> Result<Self> {
        let raw_name = name.as_ref();
        let name = HeaderName::from_bytes(raw_name.as_bytes())
            .map_err(|e| LlmError::InvalidRequest(format!("非法请求头名 {raw_name}: {e}")))?;
        let value = HeaderValue::from_str(value.as_ref())
            .map_err(|e| LlmError::InvalidRequest(format!("非法请求头值: {e}")))?;
        self.extra_headers.insert(name, value);
        Ok(self)
    }

    /// 设置 User-Agent
    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
        self
    }

    /// 设置代理（如 `http://127.0.0.1:7890`）
    pub fn with_proxy(mut self, proxy: impl Into<String>) -> Self {
        self.proxy = Some(proxy.into());
        self
    }

    /// 允许自签证书（本地自建 HTTPS 网关）
    pub fn with_accept_invalid_certs(mut self, allow: bool) -> Self {
        self.accept_invalid_certs = allow;
        self
    }

    /// 设置最大重试次数（默认 0）
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// 设置重试退避基值
    pub fn with_retry_base_delay(mut self, delay: Duration) -> Self {
        self.retry_base_delay = delay;
        self
    }

    /// 设置单次响应体大小上限
    pub fn with_max_response_bytes(mut self, bytes: usize) -> Self {
        self.max_response_bytes = bytes;
        self
    }

    /// 注册原始 SSE 调试回调
    pub fn with_debug_hook(mut self, hook: DebugHook) -> Self {
        self.debug_hook = Some(hook);
        self
    }

    /// 注入外部 `reqwest::Client`（复用调用方的连接池、代理与中间件）
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.http = Some(client);
        self
    }

    /// 基础地址
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 认证方式
    pub fn auth(&self) -> &AuthStyle {
        &self.auth
    }

    /// 是否配置了 API 密钥
    pub fn has_api_key(&self) -> bool {
        !self.api_key.is_empty()
    }

    /// 等待响应头的超时
    pub fn response_timeout(&self) -> Option<Duration> {
        self.response_timeout
    }

    /// 流式空闲超时
    pub fn stream_idle_timeout(&self) -> Option<Duration> {
        self.stream_idle_timeout
    }

    /// 非流式响应体读取超时
    pub fn total_timeout(&self) -> Option<Duration> {
        self.total_timeout
    }

    /// 最大重试次数
    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }

    /// 重试退避基值
    pub fn retry_base_delay(&self) -> Duration {
        self.retry_base_delay
    }

    /// 单次响应体大小上限
    pub fn max_response_bytes(&self) -> usize {
        self.max_response_bytes
    }

    /// 原始 SSE 调试回调
    pub fn debug_hook(&self) -> Option<&DebugHook> {
        self.debug_hook.as_ref()
    }

    /// chat completions 的完整地址
    ///
    /// `base_url` 允许带或不带末尾斜杠，也允许直接填完整的 `.../chat/completions`。
    pub fn chat_url(&self) -> String {
        if let Some(endpoint) = &self.chat_endpoint {
            return self.with_auth_query(endpoint.clone());
        }
        let base = self.base_url.trim().trim_end_matches('/');
        let url = if base.ends_with("/chat/completions") {
            base.to_string()
        } else {
            format!("{base}/chat/completions")
        };
        self.with_auth_query(url)
    }

    /// 基于 `base_url` 拼接任意子路径的完整地址
    pub fn endpoint_url(&self, path: &str) -> String {
        let base = self.base_url.trim().trim_end_matches('/');
        let url = format!("{base}/{}", path.trim_start_matches('/'));
        self.with_auth_query(url)
    }

    /// API 密钥（仅库内部与同 crate 使用，不对外暴露）
    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }

    /// 构建底层 HTTP 客户端
    ///
    /// 只在客户端级别设置「连接超时」等静态参数；
    /// 请求级超时由调用方按流式 / 非流式分别控制，避免总超时误杀长流。
    pub(crate) fn build_http(&self) -> Result<reqwest::Client> {
        if let Some(client) = &self.http {
            return Ok(client.clone());
        }

        let mut builder = reqwest::Client::builder();
        if let Some(timeout) = self.connect_timeout {
            builder = builder.connect_timeout(timeout);
        }
        if let Some(agent) = &self.user_agent {
            builder = builder.user_agent(agent.clone());
        }
        if let Some(proxy) = &self.proxy {
            builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(LlmError::Http)?);
        }
        if !self.extra_headers.is_empty() {
            builder = builder.default_headers(self.extra_headers.clone());
        }
        builder = builder.danger_accept_invalid_certs(self.accept_invalid_certs);

        builder.build().map_err(LlmError::Http)
    }

    /// 查询参数鉴权：把密钥拼到 URL 上
    fn with_auth_query(&self, url: String) -> String {
        match &self.auth {
            AuthStyle::Query(name) if !self.api_key.is_empty() => {
                let sep = if url.contains('?') { '&' } else { '?' };
                format!("{url}{sep}{name}={}", percent_encode(self.api_key.as_str()))
            }
            _ => url,
        }
    }
}

/// 对查询参数做最小限度的百分号编码
///
/// 只为查询参数鉴权服务（密钥通常是字母数字与 `-_`），
/// 避免为了一个函数引入额外依赖。
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// 手动实现 Debug：API Key 必须脱敏，避免日志泄漏
impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &mask_secret(&self.api_key))
            .field("auth", &self.auth)
            .field("chat_endpoint", &self.chat_endpoint)
            .field("connect_timeout", &self.connect_timeout)
            .field("response_timeout", &self.response_timeout)
            .field("stream_idle_timeout", &self.stream_idle_timeout)
            .field("total_timeout", &self.total_timeout)
            .field("extra_headers", &self.extra_headers.keys())
            .field("user_agent", &self.user_agent)
            .field("proxy", &self.proxy)
            .field("accept_invalid_certs", &self.accept_invalid_certs)
            .field("max_retries", &self.max_retries)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("debug_hook", &self.debug_hook.as_ref().map(|_| "<hook>"))
            .field("http", &self.http.as_ref().map(|_| "<injected>"))
            .finish()
    }
}

/// 密钥脱敏：只保留前 3 位
fn mask_secret(secret: &str) -> String {
    if secret.is_empty() {
        return String::new();
    }
    let head: String = secret.chars().take(3).collect();
    format!("{head}***({} chars)", secret.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 地址拼接：末尾斜杠、完整 endpoint、子路径都要正确
    #[test]
    fn test_url_joining() {
        let cfg = ClientConfig::new("https://api.deepseek.com/v1/", "sk-x");
        assert_eq!(
            cfg.chat_url(),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            cfg.endpoint_url("/models"),
            "https://api.deepseek.com/v1/models"
        );

        let full = ClientConfig::new("https://x.com/v1/chat/completions", "sk-x");
        assert_eq!(full.chat_url(), "https://x.com/v1/chat/completions");

        let azure = ClientConfig::new("https://x.openai.azure.com", "sk-x")
            .with_chat_endpoint("https://x.openai.azure.com/openai/deployments/d/chat/completions?api-version=2024-06-01");
        assert!(azure.chat_url().contains("/openai/deployments/d/"));
    }

    /// 查询参数鉴权：密钥需要编码，且与已有查询参数用 `&` 连接
    #[test]
    fn test_query_auth() {
        let cfg = ClientConfig::new("https://x.com/v1", "sk-a b")
            .with_auth(AuthStyle::Query("key".into()));
        assert!(cfg.chat_url().ends_with("/chat/completions?key=sk-a%20b"));

        let cfg = ClientConfig::new("https://x.com/v1", "k")
            .with_chat_endpoint("https://x.com/chat?api-version=1")
            .with_auth(AuthStyle::Query("key".into()));
        assert!(cfg.chat_url().ends_with("api-version=1&key=k"));
    }

    /// Debug 输出不能包含完整密钥
    #[test]
    fn test_debug_masks_api_key() {
        let cfg = ClientConfig::new("https://x.com/v1", "sk-super-secret-value");
        let text = format!("{cfg:?}");
        assert!(
            !text.contains("secret-value"),
            "Debug 输出泄漏了密钥: {text}"
        );
        assert!(text.contains("sk-***"));
    }

    /// 非法请求头要在构造期就被拒绝
    #[test]
    fn test_invalid_header_rejected() {
        let result = ClientConfig::new("https://x.com/v1", "k").with_header("bad header", "v");
        assert!(matches!(result, Err(LlmError::InvalidRequest(_))));
    }
}
