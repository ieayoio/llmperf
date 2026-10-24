//! 统一错误类型
//!
//! 错误被划分为几个可判别的类别，调用方据此决定「重试 / 换 key / 直接失败 / 展示给用户」：
//! - [`LlmError::Http`]：传输层（连接、TLS、DNS、读超时、连接重置）
//! - [`LlmError::Timeout`] / [`LlmError::IdleTimeout`]：超时（等待响应头 / 流中途无数据）
//! - [`LlmError::Api`]：服务端返回的业务错误（鉴权、限流、参数非法……）
//! - [`LlmError::Json`] / [`LlmError::StreamParse`]：协议解析失败
//! - [`LlmError::Canceled`]：调用方主动取消

use std::time::Duration;

use serde_json::Value;

/// 库内统一使用的错误类型
///
/// 枚举标记了 `#[non_exhaustive]`，后续新增变体不会破坏下游的 `match`。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LlmError {
    /// HTTP 传输层错误（网络不通、DNS 解析失败、TLS 握手失败、流被重置等）
    #[error("HTTP 请求错误: {0}")]
    Http(#[from] reqwest::Error),

    /// 等待响应头超过配置的超时时间
    #[error("请求超时: {0}")]
    Timeout(String),

    /// 流式读取空闲超时：服务端在约定时间内没有发送任何新数据
    #[error("流空闲超时: 超过 {idle_ms}ms 未收到数据")]
    IdleTimeout {
        /// 空闲超时阈值（毫秒）
        idle_ms: u64,
    },

    /// 调用方主动取消
    #[error("请求已取消")]
    Canceled,

    /// 响应体超过配置的大小上限，已主动中断读取
    #[error("响应体超过上限 {limit} 字节，已中断读取")]
    ResponseTooLarge {
        /// 配置的字节上限
        limit: usize,
    },

    /// JSON 序列化 / 反序列化错误
    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),

    /// API 返回的业务错误（如鉴权失败、参数非法、余额不足、限流等）
    #[error("API 错误(HTTP {status}): {message}")]
    Api {
        /// HTTP 状态码（流内错误包没有状态码，固定按 200 记录）
        status: u16,
        /// 可读的错误信息
        message: String,
        /// 服务端错误码（如 `invalid_api_key`）
        code: Option<String>,
        /// 服务端错误类型（如 `rate_limit_error`）
        error_type: Option<String>,
        /// 出错的参数名
        param: Option<String>,
        /// 服务端建议的重试等待时间（来自 `Retry-After` 或错误体）
        retry_after_ms: Option<u64>,
    },

    /// 请求参数非法（发送前校验失败，不会产生网络请求）
    #[error("请求参数错误: {0}")]
    InvalidRequest(String),

    /// SSE 数据解析错误
    #[error("流解析错误: {0}")]
    StreamParse(String),
}

/// 库内统一使用的结果类型
pub type Result<T, E = LlmError> = std::result::Result<T, E>;

impl LlmError {
    /// 构造一个只带状态码与文案的 API 错误
    pub fn api(status: u16, message: impl Into<String>) -> Self {
        Self::Api {
            status,
            message: message.into(),
            code: None,
            error_type: None,
            param: None,
            retry_after_ms: None,
        }
    }

    /// HTTP 状态码（仅 API / 流内错误包可提供）
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// 服务端业务错误码
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Api { code, .. } => code.as_deref(),
            _ => None,
        }
    }

    /// 服务端建议的重试等待时间
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Api { retry_after_ms, .. } => retry_after_ms.map(Duration::from_millis),
            _ => None,
        }
    }

    /// 是否被调用方取消
    pub fn is_canceled(&self) -> bool {
        matches!(self, Self::Canceled)
    }

    /// 是否为超时类错误
    pub fn is_timeout(&self) -> bool {
        match self {
            Self::Http(e) => e.is_timeout(),
            Self::Timeout(_) | Self::IdleTimeout { .. } => true,
            _ => false,
        }
    }

    /// 是否值得重试
    ///
    /// 只把「可能瞬时恢复」的错误视为可重试：连接/超时类错误、408/409/429 与 5xx。
    /// 解析错误与参数错误重试也不会成功，直接返回 false。
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http(e) => e.is_timeout() || e.is_connect() || e.is_request() || e.is_body(),
            Self::Timeout(_) | Self::IdleTimeout { .. } => true,
            Self::Api { status, .. } => {
                *status == 408 || *status == 409 || *status == 429 || *status >= 500
            }
            _ => false,
        }
    }
}

/// 从错误响应体（任意 JSON 形态）中提取结构化 API 错误
///
/// 兼容几种常见形态：
/// - `{"error": {"message": "...", "code": "...", "type": "..."}}`
/// - `{"error": "plain string"}`
/// - 直接把 `message` / `code` 平铺在顶层的实现
pub(crate) fn api_error_from_value(
    status: u16,
    value: &Value,
    retry_after: Option<Duration>,
) -> LlmError {
    // 定位真正的错误对象：优先取 `error` 字段，其次把「自带 message 的顶层对象」当作错误体
    let payload = match value.get("error") {
        Some(inner) => inner,
        None if value.get("message").is_some() => value,
        None => value,
    };

    let (message, code, error_type, param) = match payload {
        Value::Object(object) => (
            object
                .get("message")
                .and_then(value_to_string)
                .unwrap_or_else(|| payload.to_string()),
            object.get("code").and_then(value_to_string),
            object.get("type").and_then(value_to_string),
            object.get("param").and_then(value_to_string),
        ),
        other => (
            value_to_string(other).unwrap_or_else(|| value.to_string()),
            None,
            None,
            None,
        ),
    };

    LlmError::Api {
        status,
        message,
        code,
        error_type,
        param,
        retry_after_ms: retry_after.map(|d| d.as_millis() as u64),
    }
}

/// JSON 值转字符串（`null` 视为不存在，数字等按字面量输出）
fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 标准 OpenAI 错误体：三个字段都要被拆出来
    #[test]
    fn test_api_error_from_openai_body() {
        let value = serde_json::json!({
            "error": {
                "message": "Incorrect API key provided",
                "type": "invalid_request_error",
                "param": null,
                "code": "invalid_api_key"
            }
        });
        let err = api_error_from_value(401, &value, Some(Duration::from_secs(2)));
        match err {
            LlmError::Api {
                status,
                message,
                code,
                error_type,
                retry_after_ms,
                ..
            } => {
                assert_eq!(status, 401);
                assert_eq!(message, "Incorrect API key provided");
                assert_eq!(code.as_deref(), Some("invalid_api_key"));
                assert_eq!(error_type.as_deref(), Some("invalid_request_error"));
                assert_eq!(retry_after_ms, Some(2000));
            }
            other => panic!("应为 Api 错误，实际: {other:?}"),
        }
    }

    /// 字符串形态的错误体不能丢失信息
    #[test]
    fn test_api_error_from_plain_string() {
        let value = serde_json::json!({ "error": "model not found" });
        let err = api_error_from_value(404, &value, None);
        assert!(err.to_string().contains("model not found"));
        assert!(!err.is_retryable());
    }

    /// 重试判定：429/5xx 可重试，400/401 不可重试
    #[test]
    fn test_retryable_classification() {
        assert!(LlmError::api(429, "slow down").is_retryable());
        assert!(LlmError::api(503, "unavailable").is_retryable());
        assert!(!LlmError::api(400, "bad request").is_retryable());
        assert!(!LlmError::api(401, "unauthorized").is_retryable());
        assert!(LlmError::IdleTimeout { idle_ms: 10 }.is_retryable());
        assert!(!LlmError::Canceled.is_retryable());
        assert!(LlmError::Canceled.is_canceled());
    }
}
