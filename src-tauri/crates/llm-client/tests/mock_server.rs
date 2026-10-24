//! 基于本地 mock HTTP 服务的集成测试
//!
//! 不依赖外网，也不需要额外的测试框架：自己起一个极小 HTTP 服务，
//! 按需返回 JSON / SSE、分片发送、延迟关闭，从而验证：
//! - 请求体与请求头的构造
//! - 429 重试与 Retry-After
//! - SSE 事件解析、累加、错误包上抛、尾帧无换行
//! - 空闲超时与取消
//! - 200 + JSON 错误体、非 SSE 响应等边界

use std::sync::Arc;
use std::time::Duration;

use llm_client::{
    ChatMessage, ChatParams, ClientConfig, LlmClient, LlmError, RequestOptions, StreamEvent,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

// ============================ mock HTTP 服务 ============================

/// 一条预置响应
struct Reply {
    status: u16,
    content_type: &'static str,
    body: String,
    /// 把 body 拆成几块发送（模拟 SSE 分片与跨块的半行）
    pieces: usize,
    /// 分块之间的延迟
    delay: Duration,
    /// 写完响应后保持连接多久再关闭（模拟服务端卡住）
    hold: Duration,
    /// 声明的 Content-Length 比实际发送的字节多出的量
    ///
    /// 不声明额外长度的话，reqwest 读完 Content-Length 字节就会认为 body 结束（EOF），
    /// 测不到「服务端卡住」；声明长一点才能让读取真的阻塞。
    extra_length: usize,
    extra_headers: Vec<String>,
}

impl Reply {
    fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: body.into(),
            pieces: 1,
            delay: Duration::ZERO,
            hold: Duration::ZERO,
            extra_length: 0,
            extra_headers: Vec::new(),
        }
    }

    fn sse(body: impl Into<String>) -> Self {
        Self {
            content_type: "text/event-stream",
            ..Self::json(body)
        }
    }

    fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    fn pieces(mut self, pieces: usize) -> Self {
        self.pieces = pieces;
        self
    }

    fn hold(mut self, hold: Duration) -> Self {
        self.hold = hold;
        // 同时声明更长的 Content-Length，否则 body 会立即结束
        self.extra_length = 1024;
        self
    }

    fn header(mut self, line: impl Into<String>) -> Self {
        self.extra_headers.push(line.into());
        self
    }
}

/// 本地 mock 服务
struct MockServer {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    handle: JoinHandle<()>,
}

impl MockServer {
    /// 启动服务：第 i 个连接使用 `replies[i]`（超出后复用最后一个）
    async fn spawn(replies: Vec<Reply>) -> Self {
        assert!(!replies.is_empty(), "至少需要一条预置响应");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("端口绑定失败");
        let addr = listener.local_addr().expect("取本地地址失败");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);

        let handle = tokio::spawn(async move {
            let mut index = 0usize;
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let request = read_request(&mut stream).await.unwrap_or_default();
                captured.lock().await.push(request);

                let reply = &replies[index.min(replies.len() - 1)];
                index += 1;
                write_reply(&mut stream, reply).await;
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            requests,
            handle,
        }
    }

    /// 带 `/v1` 前缀的 API 根地址
    fn api_base(&self) -> String {
        format!("{}/v1", self.base_url)
    }

    async fn requests(&self) -> Vec<String> {
        self.requests.lock().await.clone()
    }

    async fn request_count(&self) -> usize {
        self.requests.lock().await.len()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn client_for(server: &MockServer, api_key: &str) -> LlmClient {
    LlmClient::new(ClientConfig::new(server.api_base(), api_key)).expect("客户端创建失败")
}

/// 读取完整请求（请求行 + 头 + body）
async fn read_request(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = header_end(&buffer) {
            let head = String::from_utf8_lossy(&buffer[..end]).to_string();
            if buffer.len() - end >= content_length(&head) {
                break;
            }
        }
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

fn header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

fn content_length(head: &str) -> usize {
    for line in head.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                return value.trim().parse().unwrap_or(0);
            }
        }
    }
    0
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

async fn write_reply(stream: &mut TcpStream, reply: &Reply) {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reason_phrase(reply.status),
        reply.content_type,
        reply.body.len() + reply.extra_length
    );
    for line in &reply.extra_headers {
        head.push_str(line);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    if stream.write_all(head.as_bytes()).await.is_err() {
        return;
    }

    let bytes = reply.body.as_bytes();
    let pieces = reply.pieces.max(1);
    let chunk_size = bytes.len().div_ceil(pieces).max(1);
    for piece in bytes.chunks(chunk_size) {
        if stream.write_all(piece).await.is_err() {
            return;
        }
        let _ = stream.flush().await;
        if !reply.delay.is_zero() {
            tokio::time::sleep(reply.delay).await;
        }
    }

    if !reply.hold.is_zero() {
        tokio::time::sleep(reply.hold).await;
    }
}

// ============================ 非流式调用 ============================

/// 非流式调用：请求头、请求体与响应解析
#[tokio::test]
async fn test_chat_success_and_headers() {
    let server = MockServer::spawn(vec![Reply::json(
        r#"{"id":"x","choices":[{"index":0,"message":{"content":"你好"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
    )])
    .await;

    let client = client_for(&server, "sk-test");
    let completion = client
        .chat(
            vec![ChatMessage::user("hi")],
            &ChatParams::new("test-model"),
        )
        .await
        .expect("非流式调用应成功");

    assert_eq!(completion.content(), "你好");
    assert_eq!(completion.usage.unwrap().total_tokens, 5);

    let requests = server.requests().await;
    let request = requests.first().expect("应有请求").to_lowercase();
    assert!(
        request.starts_with("post /v1/chat/completions"),
        "路径应拼在 base_url 之后"
    );
    assert!(request.contains("authorization: bearer sk-test"));
    assert!(request.contains("text/event-stream"), "应声明接受 SSE");
    assert!(request.contains("\"stream\":false"));
    assert!(request.contains("\"model\":\"test-model\""));
}

/// 429 + Retry-After：可重试错误应自动重试并最终成功
#[tokio::test]
async fn test_chat_retries_on_429() {
    let server = MockServer::spawn(vec![
        Reply::json(r#"{"error":{"message":"rate limited","code":"rate_limit_exceeded"}}"#)
            .status(429)
            .header("Retry-After: 0"),
        Reply::json(
            r#"{"choices":[{"index":0,"message":{"content":"ok"},"finish_reason":"stop"}]}"#,
        ),
    ])
    .await;

    let client = LlmClient::new(
        ClientConfig::new(server.api_base(), "sk-test")
            .with_max_retries(1)
            .with_retry_base_delay(Duration::from_millis(1)),
    )
    .expect("客户端创建失败");

    let completion = client
        .chat(vec![ChatMessage::user("hi")], &ChatParams::new("m"))
        .await
        .expect("重试后应成功");
    assert_eq!(completion.content(), "ok");
    assert_eq!(server.request_count().await, 2, "应发生一次重试");
}

/// 不支持 stream_options 的老网关：应去掉该字段降级重试一次
#[tokio::test]
async fn test_stream_options_fallback() {
    let server = MockServer::spawn(vec![
        Reply::json(r#"{"error":{"message":"unknown field: stream_options"}}"#).status(400),
        Reply::sse(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n",
        ),
    ])
    .await;

    let client = client_for(&server, "sk-test");
    let stream = client
        .chat_stream(vec![ChatMessage::user("hi")], &ChatParams::new("m"))
        .await
        .expect("降级后应成功");
    let accumulator = stream.drain().await.expect("降级后应能解析");
    assert_eq!(accumulator.content(), "ok");

    let requests = server.requests().await;
    assert_eq!(requests.len(), 2, "应降级重试一次");
    assert!(requests[0].contains("stream_options"));
    assert!(
        !requests[1].contains("stream_options"),
        "降级请求不应再带该字段"
    );
}

/// 不可重试的 401 只发一次请求
#[tokio::test]
async fn test_chat_does_not_retry_on_401() {
    let server = MockServer::spawn(vec![Reply::json(
        r#"{"error":{"message":"bad key","code":"invalid_api_key"}}"#,
    )
    .status(401)])
    .await;

    let client = LlmClient::new(
        ClientConfig::new(server.api_base(), "sk-bad")
            .with_max_retries(3)
            .with_retry_base_delay(Duration::from_millis(1)),
    )
    .expect("客户端创建失败");

    let error = client
        .chat(vec![ChatMessage::user("hi")], &ChatParams::new("m"))
        .await
        .expect_err("应返回鉴权错误");
    match error {
        LlmError::Api { status, code, .. } => {
            assert_eq!(status, 401);
            assert_eq!(code.as_deref(), Some("invalid_api_key"));
        }
        other => panic!("应为 Api 错误，实际: {other:?}"),
    }
    assert_eq!(server.request_count().await, 1, "401 不应重试");
}

/// HTTP 200 但响应体是错误对象：必须识别为错误
#[tokio::test]
async fn test_chat_200_with_error_body() {
    let server = MockServer::spawn(vec![Reply::json(
        r#"{"error":{"message":"quota exceeded","type":"insufficient_quota"}}"#,
    )])
    .await;

    let error = client_for(&server, "sk-test")
        .chat(vec![ChatMessage::user("hi")], &ChatParams::new("m"))
        .await
        .expect_err("200 + 错误体应报错");
    assert!(
        error.to_string().contains("quota exceeded"),
        "实际: {error}"
    );
}

/// 多模态数组形态的 content 必须降级为文本
#[tokio::test]
async fn test_chat_content_array() {
    let server = MockServer::spawn(vec![Reply::json(
        r#"{"choices":[{"index":0,"message":{"content":[{"type":"text","text":"第一段"},{"type":"text","text":"第二段"}]}}]}"#,
    )])
    .await;

    let completion = client_for(&server, "sk-test")
        .chat(vec![ChatMessage::user("hi")], &ChatParams::new("m"))
        .await
        .expect("应成功");
    assert_eq!(completion.content(), "第一段第二段");
}

/// 模型列表接口
#[tokio::test]
async fn test_list_models() {
    let server = MockServer::spawn(vec![Reply::json(
        r#"{"data":[{"id":"m1","owned_by":"x"},{"id":"m2"}]}"#,
    )])
    .await;

    let models = client_for(&server, "sk-test")
        .list_models()
        .await
        .expect("应能获取模型列表");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "m1");

    let requests = server.requests().await;
    assert!(requests[0].starts_with("GET /v1/models"));
}

// ============================ 流式调用 ============================

/// 流式：思考、正文、工具调用、usage、[DONE] 全都要正确累积
#[tokio::test]
async fn test_stream_accumulates_everything() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"想一下\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你好\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"北京\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":9,\"total_tokens\":14}}\n\n",
        "data: [DONE]\n\n",
    );
    // 切成 9 块，让行跨 chunk 断开
    let server = MockServer::spawn(vec![Reply::sse(sse).pieces(9)]).await;

    let client = client_for(&server, "sk-test");
    let stream = client
        .chat_stream(vec![ChatMessage::user("北京天气")], &ChatParams::new("m"))
        .await
        .expect("流式请求应能发出");
    let accumulator = stream.drain().await.expect("流应完整解析");

    assert_eq!(accumulator.reasoning_content(), "想一下");
    assert_eq!(accumulator.content(), "你好");
    assert_eq!(accumulator.finish_reason(), Some("tool_calls"));
    assert_eq!(accumulator.usage.as_ref().unwrap().total_tokens, 14);
    assert!(accumulator.is_complete(), "收到 [DONE] 应视为完整");
    assert!(
        accumulator.first_token_at.is_some(),
        "首 token 时刻应被打点"
    );

    let calls = accumulator.tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].function.name, "get_weather");
    assert_eq!(calls[0].function.arguments, r#"{"city":"北京"}"#);
}

/// 流内错误包必须变成错误，而不是静默的空回复
#[tokio::test]
async fn test_stream_error_chunk_reported() {
    let server = MockServer::spawn(vec![Reply::sse(
        "data: {\"error\":{\"message\":\"overloaded\",\"code\":\"server_error\"}}\n\n",
    )])
    .await;

    let stream = client_for(&server, "sk-test")
        .chat_stream(vec![ChatMessage::user("hi")], &ChatParams::new("m"))
        .await
        .expect("建连应成功");

    match stream.drain().await {
        Err(LlmError::Api { message, code, .. }) => {
            assert_eq!(message, "overloaded");
            assert_eq!(code.as_deref(), Some("server_error"));
        }
        other => panic!("应为 Api 错误，实际: {other:?}"),
    }
}

/// 200 + JSON（不是 SSE）：必须报错并带上服务端信息
#[tokio::test]
async fn test_stream_json_response_rejected() {
    let server = MockServer::spawn(vec![Reply::json(
        r#"{"error":{"message":"model not found"}}"#,
    )])
    .await;

    let error = client_for(&server, "sk-test")
        .chat_stream(vec![ChatMessage::user("hi")], &ChatParams::new("m"))
        .await
        .expect_err("非 SSE 响应应报错");
    assert!(
        error.to_string().contains("model not found"),
        "实际: {error}"
    );
}

/// 最后一帧没有尾随换行时不能丢内容
#[tokio::test]
async fn test_stream_trailing_frame_without_newline() {
    let server = MockServer::spawn(vec![Reply::sse(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"尾帧\"}}]}",
    )])
    .await;

    let stream = client_for(&server, "sk-test")
        .chat_stream(vec![ChatMessage::user("hi")], &ChatParams::new("m"))
        .await
        .expect("流式请求应能发出");
    let accumulator = stream.drain().await.expect("应能解析尾帧");

    assert_eq!(accumulator.content(), "尾帧");
    // 没有 [DONE] 也没有 finish_reason，应判定为不完整
    assert!(!accumulator.is_complete());
}

/// 服务端中途不再发送数据：空闲超时必须生效并给出可判别错误
#[tokio::test]
async fn test_stream_idle_timeout() {
    let server = MockServer::spawn(vec![Reply::sse(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"开头\"}}]}\n\n",
    )
    .hold(Duration::from_secs(5))])
    .await;

    let client = LlmClient::new(
        ClientConfig::new(server.api_base(), "sk-test")
            .with_stream_idle_timeout(Some(Duration::from_millis(150))),
    )
    .expect("客户端创建失败");

    let mut stream = client
        .chat_stream(vec![ChatMessage::user("hi")], &ChatParams::new("m"))
        .await
        .expect("流式请求应能发出");

    let mut saw_content = false;
    let mut timed_out = false;
    while let Some(event) = stream.recv().await {
        match event {
            Ok(StreamEvent::ContentDelta { .. }) => saw_content = true,
            Err(LlmError::IdleTimeout { .. }) => {
                timed_out = true;
                break;
            }
            Ok(_) => {}
            Err(other) => panic!("意外错误: {other:?}"),
        }
    }
    assert!(saw_content, "超时前应已收到正文");
    assert!(timed_out, "应返回 IdleTimeout");
}

/// 取消令牌要能立刻打断阻塞中的读取
#[tokio::test]
async fn test_stream_cancel_token() {
    let server = MockServer::spawn(vec![Reply::sse(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"}}]}\n\n",
    )
    .hold(Duration::from_secs(5))])
    .await;

    let client = client_for(&server, "sk-test");
    let token = CancellationToken::new();
    let mut stream = client
        .chat_stream_with(
            &[ChatMessage::user("hi")],
            &ChatParams::new("m"),
            RequestOptions::new().with_cancel(token.clone()),
        )
        .await
        .expect("流式请求应能发出");

    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    });

    let mut canceled = false;
    while let Some(event) = stream.recv().await {
        if let Err(error) = event {
            canceled = error.is_canceled();
            break;
        }
    }
    canceller.await.expect("取消任务执行失败");
    assert!(canceled, "取消应尽快生效");
}

/// 丢弃流对象应关闭连接（服务端在 hold 期间会被中断，不会无限占用）
#[tokio::test]
async fn test_dropping_stream_stops_reading() {
    let server = MockServer::spawn(vec![Reply::sse(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"}}]}\n\n",
    )
    .hold(Duration::from_secs(5))])
    .await;

    let mut stream = client_for(&server, "sk-test")
        .chat_stream(vec![ChatMessage::user("hi")], &ChatParams::new("m"))
        .await
        .expect("流式请求应能发出");

    // 读一个事件后直接丢弃
    let first = stream.recv().await;
    assert!(matches!(first, Some(Ok(StreamEvent::ContentDelta { .. }))));
    drop(stream);

    // 连接被关闭后，服务端读取会很快结束（这里只验证不 panic 且能及时完成）
    let done = tokio::time::timeout(Duration::from_secs(2), async {
        server.request_count().await
    })
    .await;
    assert!(done.is_ok());
}
