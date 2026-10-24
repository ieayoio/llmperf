# llm-client

OpenAI 兼容的大模型客户端（Rust）。零 GUI 依赖，可独立复用到任意 Rust 项目。

本 crate 从 [llmperf](../../../README.md) 中抽离而来：应用层只负责界面与编排，HTTP 传输、SSE 解析、
取消、重试与指标计算全部收在这里。

```
llm-client
├── 传输：reqwest（连接复用、gzip/brotli、代理、自签证书）
├── 协议：chat/completions 非流式 + SSE 流式、models、embeddings、任意 JSON 逃生口
├── 能力：工具调用、多模态、多候选、思考内容、结构化输出
└── 指标：TTFT / 首字延迟 / TPOT / ITL / token 速度
```

- 版本：`0.1.0`
- 依赖：`reqwest` / `tokio` / `serde` / `serde_json` / `thiserror` / `bytes` / `futures-util` / `tokio-util`
- 无 Cargo feature 开关（默认即全量能力）

## 目录

- [特性](#特性)
- [安装](#安装)
- [快速开始](#快速开始)
- [客户端配置](#客户端配置)
- [请求参数](#请求参数)
- [消息模型](#消息模型)
- [流式事件](#流式事件)
- [性能指标](#性能指标)
- [错误处理](#错误处理)
- [其他接口](#其他接口)
- [厂商兼容性](#厂商兼容性)
- [测试与示例](#测试与示例)
- [目录结构](#目录结构)
- [设计约定](#设计约定)

## 特性

| 能力 | 说明 |
| --- | --- |
| 非流式调用 | 一次返回正文、思考内容、工具调用、usage、timings |
| 流式调用 | SSE 逐增量推送；返回对象实现 `futures_util::Stream` |
| 思考内容 | 同时识别 `reasoning_content` 与 `reasoning` 两种字段名 |
| 工具调用 | 请求侧工具定义 + 流式 `tool_calls` 增量拼接 + 多轮结果回传 |
| 多模态 | `content` 支持纯文本或多段（文本 / 图片 URL） |
| 多候选 | `n > 1` 时事件带 `index`，累加器按候选分别保存 |
| 取消 | `CancellationToken` 传入即可，建连与阻塞读取都能立即打断；丢弃流对象也会关闭连接 |
| 重试 | 可配置次数 + 指数退避 + 服务端 `Retry-After`，只重试瞬时错误 |
| 超时 | 连接 / 等待响应头 / 流空闲 / 非流式读 body 四段独立控制 |
| 错误 | 结构化：HTTP 状态码 + 业务码 + 错误类型 + 参数名 + 建议重试时间 |
| 安全 | API Key 在 `Debug` 输出中自动脱敏；响应体读取有大小上限 |
| 指标 | TTFT、首字延迟、TPOT、ITL 分位数、token/s（含 timings 优先与退化口径） |
| 其他接口 | `GET /models`、`POST /embeddings`、`post_json` 通用逃生口 |

## 安装

### 作为路径依赖（当前推荐）

本 crate 位于 llmperf 仓库内，跨项目直接引用路径即可：

```toml
[dependencies]
llm-client = { path = "../llmperf/src-tauri/crates/llm-client" }
```

### 复制到自己的 workspace

```toml
# 你的 workspace 根 Cargo.toml
[workspace]
members = ["crates/llm-client"]

# 需要它的 crate
[dependencies]
llm-client = { path = "crates/llm-client" }
```

依赖已保持最小化，不引入任何 Tauri / GUI 相关包。

## 快速开始

### 非流式调用

```rust
use llm_client::{ChatMessage, ChatParams, ClientConfig, LlmClient};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = LlmClient::new(ClientConfig::new(
        "https://api.deepseek.com/v1",
        std::env::var("LLM_API_KEY")?,
    ))?;

    let params = ChatParams::new("deepseek-chat")
        .with_temperature(0.2)
        .with_max_tokens(512);

    let reply = client
        .chat(vec![ChatMessage::user("用一句话解释什么是机器学习")], &params)
        .await?;

    println!("{}", reply.content());
    println!("finish_reason = {:?}", reply.finish_reason());
    if let Some(usage) = &reply.usage {
        println!("tokens: {} / {}", usage.prompt_tokens, usage.completion_tokens);
    }
    Ok(())
}
```

### 流式调用

```rust
use llm_client::{ChatMessage, ChatParams, ClientConfig, LlmClient, StreamEvent};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = LlmClient::new(ClientConfig::new("https://api.openai.com/v1", "sk-xxx"))?;
    let params = ChatParams::new("gpt-4o");

    let mut stream = client
        .chat_stream(vec![ChatMessage::user("你好")], &params)
        .await?;

    while let Some(event) = stream.recv().await {
        match event? {
            StreamEvent::ReasoningDelta { text, .. } => print!("[思考]{text}"),
            StreamEvent::ContentDelta { text, .. } => print!("{text}"),
            StreamEvent::ToolCallDelta { .. } => {}
            StreamEvent::Finish(info) => println!("\n结束: reason={:?}", info.reason),
            // StreamEvent 是 non_exhaustive，务必保留通配分支
            _ => {}
        }
    }
    Ok(())
}
```

只想拿最终结果时用聚合器：

```rust
let mut stream = client.chat_stream(messages, &params).await?;
let mut acc = llm_client::StreamAccumulator::default();
while let Some(event) = stream.recv().await {
    acc.feed(&event?);
}
assert!(!acc.content().is_empty());
println!("思考: {}", acc.reasoning_content());
println!("是否完整: {}", acc.is_complete()); // 收到 finish_reason 或 [DONE]
```

或者一步到位：

```rust
let acc = client.chat_stream(messages, &params).await?.drain().await?;
```

> `drain()` 中途出错会直接返回 `Err`（已累积内容不保留）。
> 需要「出错也要拿到半截内容」时，请手动 `recv()` + `feed()`。

### 工具调用（完整回合）

```rust
use llm_client::{ChatMessage, ChatParams, ClientConfig, LlmClient, Tool, ToolChoice};
use serde_json::json;

let client = LlmClient::new(ClientConfig::new("https://api.openai.com/v1", "sk-xxx"))?;

let params = ChatParams::new("gpt-4o")
    .with_tools(vec![Tool::function(
        "get_weather",
        "查询城市天气",
        json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
    )])
    .with_tool_choice(ToolChoice::auto());

// 1) 第一轮：模型返回 tool_calls
let question = ChatMessage::user("北京今天天气怎么样？");
let first = client.chat(vec![question.clone()], &params).await?;
let calls = first.tool_calls().to_vec();
```

拿到 `calls` 后执行真实工具，再把结果回传：

```rust
let mut messages = vec![question, ChatMessage::assistant_with_tool_calls(calls.clone())];
for call in &calls {
    // call.function.name / call.function.arguments（JSON 字符串）
    messages.push(ChatMessage::tool(&call.id, r#"{"temp_c":20,"desc":"晴"}"#));
}
let answer = client.chat(messages, &params).await?;
println!("{}", answer.content());
```

流式场景不需要自己拼 JSON 片段，`StreamAccumulator` 会按 `tool_calls[].index` 累积：

```rust
let acc = client.chat_stream(messages, &params).await?.drain().await?;
for call in acc.tool_calls() {
    println!("{} -> {}", call.function.name, call.function.arguments);
}
```

## 客户端配置

`ClientConfig` 字段全部私有，通过 `with_*` 链式配置（新增字段不会破坏调用方）。

### 基础

```rust
use llm_client::{AuthStyle, ClientConfig};
use std::time::Duration;

let config = ClientConfig::new("https://api.openai.com/v1", "sk-xxx")
    // 代理与自定义头（with_header 返回 Result，可失败）
    .with_proxy("http://127.0.0.1:7890")
    .with_header("HTTP-Referer", "https://example.com")?
    .with_user_agent("my-app/1.0")
    // 自建网关自签证书
    .with_accept_invalid_certs(true)
    // 复用调用方已经配置好的连接池 / 中间件
    // .with_http_client(existing_reqwest_client)
    ;
```

无认证的本地服务：

```rust
let config = ClientConfig::anonymous("http://127.0.0.1:8000/v1");
```

### 认证方式

| 方式 | 构造 | 发出的请求 |
| --- | --- | --- |
| Bearer（默认） | `ClientConfig::new(url, key)` | `Authorization: Bearer <key>` |
| 自定义头 | `.with_auth(AuthStyle::Header("api-key".into()))` | `api-key: <key>`（Azure OpenAI） |
| 查询参数 | `.with_auth(AuthStyle::Query("key".into()))` | `?key=<key>` |
| 无认证 | `ClientConfig::anonymous(url)` | 不发送凭据 |

### 三级超时

| 配置 | 默认值 | 作用范围 |
| --- | --- | --- |
| `with_connect_timeout` | 10s | TCP / TLS 握手 |
| `with_response_timeout` | 600s | 发出请求 → 收到响应头（流式下即 prefill 上限） |
| `with_stream_idle_timeout` | 120s | 流式读取时**相邻两块数据**之间的最大间隔，`None` 表示不限制 |
| `with_total_timeout` | 600s | 非流式响应体的整体读取时间，`None` 表示不限制 |

要点：**没有「整个流的总超时」**。推理模型输出几分钟很正常，用总超时会误杀长流；
真正的「卡死」由流空闲超时兜底。

```rust
let config = ClientConfig::new(url, key)
    .with_connect_timeout(Duration::from_secs(5))
    .with_response_timeout(Duration::from_secs(300))
    .with_stream_idle_timeout(Some(Duration::from_secs(30)))
    .with_total_timeout(Some(Duration::from_secs(120)));
```

### 重试

```rust
let config = ClientConfig::new(url, key)
    .with_max_retries(2)                             // 默认 0
    .with_retry_base_delay(Duration::from_millis(300));
```

- 只重试**瞬时**错误：连接失败 / 超时 / 408 / 409 / 429 / 5xx；参数错误、鉴权失败、解析错误不重试。
- 退避为 `base * 2^(n-1)`，单次最多 8s；服务端给了 `Retry-After` 时优先采用（同样封顶 8s）。
- 流式重试只发生在「等待响应头」阶段（此时还没消费任何数据），不会重复输出内容。
- 压测场景建议保持 0，避免两次请求耗时叠加污染统计。

单次请求可覆盖：

```rust
use llm_client::RequestOptions;

let options = RequestOptions::new()
    .with_cancel(token)                     // 取消信号
    .with_max_retries(0)                    // 覆盖客户端设置
    .with_total_timeout(Some(Duration::from_secs(60)));
```

### 其他

| 方法 | 说明 |
| --- | --- |
| `with_base_url` | 修改基础地址（末尾斜杠可有可无） |
| `with_chat_endpoint` | 直接指定 chat 完整 URL（Azure 这类特殊路径） |
| `with_api_key` | 更换密钥 |
| `with_max_response_bytes` | 单次响应体上限，默认 32 MiB，超限直接报错 |
| `with_debug_hook` | 回调每条 SSE `data:` 原始负载，排查厂商差异时用 |

## 请求参数

`ChatParams` 实现 `Serialize` / `Deserialize`，既能代码构造，也能从 JSON 配置加载。

```rust
let params = ChatParams::new("qwen-plus")
    .with_temperature(0.3)
    .with_max_tokens(1024)
    .with_extra("top_k", serde_json::json!(20));   // 厂商特有字段，平铺到请求体顶层
```

常用字段（`None` 表示不传，交给服务端默认值）：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `model` | `String` | 必填 |
| `temperature` / `top_p` | `Option<f32>` | 采样 |
| `max_tokens` / `max_completion_tokens` | `Option<u32>` | 输出长度上限 |
| `frequency_penalty` / `presence_penalty` | `Option<f32>` | 惩罚项 |
| `seed` | `Option<i64>` | 可复现（需服务端支持） |
| `stop` | `Option<Vec<String>>` | 停止词 |
| `n` | `Option<u32>` | 候选数量 |
| `tools` / `tool_choice` / `parallel_tool_calls` | — | 工具调用 |
| `response_format` | `Option<ResponseFormat>` | 结构化输出 |
| `logprobs` / `top_logprobs` | — | 对数概率 |
| `user` | `Option<String>` | 调用方标识 |
| `stream_options` | `StreamOptions` | 是否在流末尾要 usage |
| `extra` | `Map<String, Value>` | 任意扩展字段 |

结构化输出：

```rust
use llm_client::ResponseFormat;

let params = ChatParams::new("gpt-4o").with_response_format(ResponseFormat::JsonObject);
// 或指定 Schema
let params = ChatParams::new("gpt-4o").with_response_format(ResponseFormat::JsonSchema {
    json_schema: serde_json::json!({
        "name": "weather",
        "schema": { "type": "object", "properties": { "temp": { "type": "number" } } }
    }),
});
```

关闭流式 usage（少数老网关不认识 `stream_options` 时）：

```rust
let params = ChatParams::new("m").without_stream_usage();
```

> 即使不关闭，库在遇到「400/422 且报错提到 `stream_options`」时也会自动去掉该字段再试一次。

`messages` / `stream` 由库统一写入请求体，`extra` 里的同名字段会被覆盖，避免发出自相矛盾的请求。

## 消息模型

```rust
use llm_client::{ChatMessage, ContentPart, ImageUrl, MessageContent};
```

| 场景 | 构造 |
| --- | --- |
| 系统提示 | `ChatMessage::system("...")` |
| 用户输入 | `ChatMessage::user("...")` |
| 助手回复 | `ChatMessage::assistant("...")` |
| 带推理链的助手消息 | `ChatMessage::assistant_with_reasoning(content, reasoning)` |
| 只带工具调用的助手消息 | `ChatMessage::assistant_with_tool_calls(calls)` |
| 工具结果 | `ChatMessage::tool(call_id, content)` |
| 多模态（图文） | `ChatMessage::user_with_parts(vec![...])` |

多模态示例：

```rust
let msg = ChatMessage::user_with_parts(vec![
    ContentPart::Text { text: "这张图里有什么？".into() },
    ContentPart::ImageUrl { image_url: ImageUrl::new("https://example.com/a.png") },
]);
```

正文形态 `MessageContent`：

- `Text(String)` → 序列化为字符串
- `Parts(Vec<ContentPart>)` → 序列化为数组
- `None` → 序列化为 `null`（配合 `tool_calls` 使用）

接收响应时，`content` 无论是字符串、数组还是 `null` 都能解析，数组会被拼成纯文本。

## 流式事件

一次分片可能同时携带多路内容，因此事件是「按语义拆开」的：

| 事件 | 携带内容 |
| --- | --- |
| `ReasoningDelta { index, text }` | 思考/推理增量 |
| `ContentDelta { index, text }` | 正文增量 |
| `ToolCallDelta { index, delta }` | 工具调用增量（`id`/`name` 只来一次，`arguments` 是片段） |
| `Finish(FinishInfo)` | 结束信息，可能多次到达 |

`StreamEvent::index()` 取候选序号，`text()` 取文本增量。

### `Finish` 为什么会来多次

不同实现把结束信息拆在不同 chunk 里（`finish_reason` 一个、`usage` 一个、`[DONE]` 又一个），
所以库按字段合并，调用方不要用「最后一个 Finish」覆盖前面的：

| 字段 | 含义 |
| --- | --- |
| `reason` | `stop` / `length` / `tool_calls`…（usage 尾包与 `[DONE]` 为空串） |
| `index` | 该 `finish_reason` 属于哪个候选 |
| `usage` | token 统计 |
| `timings` | 服务端给出的真实速度（vLLM 等） |
| `first_token_at` | **解析层**打点的首增量时刻，用于算 prefill |
| `done` | 是否由 `[DONE]` 产生 |

判断「是否正常结束」统一用 `StreamAccumulator::is_complete()`（收到 `finish_reason` 或 `[DONE]`）；
两者都没有说明连接被中途截断，正文大概率不完整。

### 多候选

`n > 1` 时，每个候选各自累积：

```rust
let acc = client.chat_stream(messages, &params).await?.drain().await?;
println!("候选0: {}", acc.content());                  // 序号最小的候选
for choice in acc.choices() {
    println!("#{} {} ({:?})", choice.index(), choice.content(), choice.finish_reason());
}
```

### 取消

三种方式，效果都是「尽快停止并释放连接」：

```rust
use llm_client::RequestOptions;
use tokio_util::sync::CancellationToken;

let token = CancellationToken::new();
let mut stream = client
    .chat_stream_with(&messages, &params, RequestOptions::new().with_cancel(token.clone()))
    .await?;

// 别处触发
token.cancel();

while let Some(event) = stream.recv().await {
    match event {
        Ok(_) => {}
        Err(e) if e.is_canceled() => break,
        Err(e) => return Err(e.into()),
    }
}
```

1. `RequestOptions::with_cancel(token)`：建连阶段、阻塞读取阶段都能立刻打断；
2. 直接 `drop(stream)`：底层连接随流一起关闭；
3. 非流式调用则 drop 对应的 `future`。

### 调试原始数据

```rust
use std::sync::Arc;

let config = ClientConfig::new(url, key).with_debug_hook(Arc::new(|raw: &str| {
    eprintln!("[sse] {raw}");
}));
```

## 性能指标

`llm_client::metrics` 与 `StreamTiming` 提供压测口径的通用实现。

| 指标 | 定义 | 获取方式 |
| --- | --- | --- |
| TTFT | 发起请求 → 首个增量（**思考或正文**）到达 | `timing.first_token_ms` / `FinishInfo::first_token_at` |
| 首字延迟 | 发起请求 → 首个**正文**增量到达 | `timing.first_content_ms` |
| TPOT | 相邻增量间隔均值（一分片一 token 时即真 TPOT） | `timing.itl().0` |
| ITL | 间隔的 p50 / p95 / max | `timing.itl().1` |
| completion tok/s | 优先 `timings.predicted_per_second`，否则 `completion_tokens / (总耗时 - prefill)` | `timing.tokens_per_second(...)` |
| prompt tok/s | 优先 `timings.prompt_per_second`，否则 `uncached_prompt_tokens / prefill` | 同上 |

两个容易踩的坑，库已经处理好：

1. **prefill 锚点必须是「首个增量」而不是「首个正文」**。推理模型先输出很长的思考内容，
   用正文时刻切分会把思考时间算进 prefill，导致 completion tok/s 高估 2~5 倍。
2. **`prompt_tokens` 包含 cache 命中部分**，直接拿来反推会偏高，退化路径统一用
   `usage.uncached_prompt_tokens()`；拿不到 cache 明细时按未命中处理。

拿不到可靠数据时统一返回 `None`（而不是 0），避免污染汇总。

```rust
use std::time::Instant;
use llm_client::{StreamAccumulator, StreamTiming};

let start = Instant::now();
let mut stream = client.chat_stream(messages, &params).await?;
let mut acc = StreamAccumulator::default();
let mut timing = StreamTiming::new(start);

while let Some(event) = stream.recv().await {
    let event = event?;
    timing.observe(&event);
    acc.feed(&event);
}

let (completion_tps, prompt_tps) =
    timing.tokens_per_second(acc.timings.as_ref(), acc.usage.as_ref());
let (tpot_ms, itl) = timing.itl();

println!("总耗时 {} ms", timing.total_ms());
println!("TTFT {:?} / 首字 {:?}", timing.first_token_ms, timing.first_content_ms);
println!("TPOT {tpot_ms:?} ms, ITL {itl:?}");
println!("completion {completion_tps:?} tok/s, prefill {prompt_tps:?} tok/s");
```

只想用纯函数（已有自己的计时逻辑）：

```rust
use llm_client::{calc_itl_stats, calc_tokens_per_second, percentile, tokens_per_second};
```

- `calc_itl_stats(&[u128]) -> (Option<f64>, Option<ItlStats>)`
- `calc_tokens_per_second(timings, usage, total_ms, first_token_ms) -> (Option<f64>, Option<f64>)`
- `tokens_per_second(tokens, duration_ms) -> Option<f64>`
- `percentile(sorted, p)`：线性插值，与 numpy 默认口径一致

## 错误处理

```rust
use llm_client::LlmError;
```

| 变体 | 含义 | 是否可重试 |
| --- | --- | --- |
| `Http(reqwest::Error)` | 连接失败、TLS、DNS、流被重置 | 视具体错误 |
| `Timeout(String)` | 等响应头超时 | 是 |
| `IdleTimeout { idle_ms }` | 流中途长时间无数据 | 是 |
| `Canceled` | 调用方取消 | 否 |
| `ResponseTooLarge { limit }` | 响应体超过上限 | 否 |
| `Json(serde_json::Error)` | 序列化 / 反序列化失败 | 否 |
| `Api { status, message, code, error_type, param, retry_after_ms }` | 服务端业务错误 / 流内错误包 | 408/409/429/5xx |
| `InvalidRequest(String)` | 发送前参数校验失败（不会产生网络请求） | 否 |
| `StreamParse(String)` | SSE 数据格式错误 | 否 |

```rust
match client.chat(messages, &params).await {
    Ok(reply) => println!("{}", reply.content()),
    Err(LlmError::Api { status: 401, .. }) => eprintln!("API Key 无效"),
    Err(LlmError::Api { status: 429, retry_after_ms, .. }) => {
        eprintln!("限流，建议 {retry_after_ms:?} ms 后重试")
    }
    Err(e) if e.is_canceled() => eprintln!("已取消"),
    Err(e) if e.is_timeout() => eprintln!("超时: {e}"),
    Err(e) => eprintln!("失败: {e}"),
}
```

- `LlmError` 标记了 `#[non_exhaustive]`，匹配时保留通配分支。
- `e.is_retryable()` 给出统一的重试建议。
- `e.status()` / `e.code()` / `e.retry_after()` 便于做日志与告警分级。

## 其他接口

```rust
// 模型列表
let models = client.list_models().await?;
for m in models {
    println!("{} ({:?})", m.id, m.owned_by);
}

// 文本向量化
use llm_client::EmbeddingParams;
let resp = client.embeddings(&EmbeddingParams::text("text-embedding-3-small", "你好")).await?;
println!("维度={}", resp.data[0].embedding.len());

// 批量
let resp = client
    .embeddings(&EmbeddingParams::texts("text-embedding-3-small", ["第一段", "第二段"]))
    .await?;

// 未封装的接口：复用同一套认证 / 重试 / 超时 / 错误解析
let value: serde_json::Value = client.post_json("chat/completions", &serde_json::json!({})).await?;
```

## 厂商兼容性

| 场景 | 处理方式 |
| --- | --- |
| `base_url` 带 / 不带末尾斜杠 | 自动归一化；也接受直接填完整的 `.../chat/completions` |
| Azure OpenAI（`api-key` 头 + deployment 路径） | `AuthStyle::Header("api-key")` + `with_chat_endpoint(完整 URL)` |
| 网关用查询参数鉴权 | `AuthStyle::Query("key")`，值自动百分号编码 |
| 网关压缩 SSE（gzip / brotli） | 已启用 `reqwest` 的 gzip/brotli 特性，自动解压 |
| 老网关不认 `stream_options` | 可手动 `without_stream_usage()`；库也会在 400/422 时自动降级重试一次 |
| 思考字段叫 `reasoning` 而不是 `reasoning_content` | 两个字段都识别，且不会重复派发 |
| 响应 `content` 是数组 | 自动拼成纯文本，不会导致解析失败 |
| usage / timings 字段缺失或为 `null` | 按默认值处理，不会让整条响应失败 |
| vLLM 的 `timings`（含 `cache_n`） | 解析并优先作为 token 速度来源 |
| 流内返回 `{"error": ...}` | 上抛为 `LlmError::Api`，不会静默变成空回复 |
| HTTP 200 但响应体是 `{"error": ...}` | 同上，非流式与流式都会识别 |
| SSE 最后一帧没有尾随换行 | EOF 时补解析，不丢内容 |
| 服务端返回 200 + JSON（不是 SSE） | 按 Content-Type 判定，直接报错并带上服务端信息 |

已知限制：

- SSE 只处理**单行** `data:` 负载（主流 OpenAI 兼容服务都如此），不支持跨行 `data:` 字段拼接。
- 不支持旧版 `/completions` 与新的 `/responses` 接口，后者可用 `post_json` 自行调用。
- 未内置限流（RPM/TPM）与并发闸门，需要时在调用方用 `Semaphore` 控制。

## 测试与示例

```bash
# 单元测试（纯解析、配置、指标）+ 本地 mock HTTP 集成测试，全程离线
cargo test -p llm-client

# 真实 API 联调：未设置环境变量时自动跳过
export LLMPERF_TEST_BASE_URL="http://127.0.0.1:16777/v1"
export LLMPERF_TEST_API_KEY="sk-xxx"
export LLMPERF_TEST_MODEL_CHAT="testllm-nk"
export LLMPERF_TEST_MODEL_REASONER="testllm"
cargo test -p llm-client --test live_api
```

`tests/mock_server.rs` 自带一个极小 HTTP 服务，用来覆盖真实网络里不好复现的场景：
请求头/请求体、429 重试与 `Retry-After`、401 不重试、`stream_options` 降级、
SSE 跨块半行、流内错误包、尾帧无换行、空闲超时、取消；

可运行示例：

```bash
LLM_BASE_URL="http://127.0.0.1:16777/v1" \
LLM_API_KEY="sk-xxx" LLM_MODEL="testllm-nk" \
  cargo run -p llm-client --example chat

LLM_BASE_URL="http://127.0.0.1:16777/v1" \
LLM_API_KEY="sk-xxx" LLM_MODEL="testllm" \
  cargo run -p llm-client --example stream
```

## 目录结构

```
crates/llm-client/
├── src/
│   ├── lib.rs        # crate 文档与导出面
│   ├── client.rs     # LlmClient：请求构造、重试、取消、流式适配
│   ├── config.rs     # ClientConfig：认证、分级超时、代理、调试钩子
│   ├── error.rs      # LlmError：结构化错误与重试判定
│   ├── message.rs    # ChatMessage / MessageContent / 多模态 / 工具调用
│   ├── params.rs     # ChatParams / Tool / ResponseFormat / 向量化参数
│   ├── response.rs   # ChatCompletion / Usage / Timings / 模型列表 / 向量化响应
│   ├── sse.rs        # SSE 解码器（纯内存，无 I/O）
│   ├── stream.rs     # StreamEvent / FinishInfo / 累加器 / ChatStream
│   └── metrics.rs    # TTFT / TPOT / ITL / token 速度
├── tests/
│   ├── mock_server.rs # 本地 mock HTTP 集成测试
│   └── live_api.rs    # 真实 API 联调（env 缺失自动跳过）
├── examples/
│   ├── chat.rs
│   └── stream.rs
└── Cargo.toml
```

## 设计约定

几条会影响使用方式的取舍，写在这里避免踩坑：

1. **`messages` / `stream` 由库写入**，`ChatParams::extra` 中的同名字段会被覆盖。
2. **流式没有总超时**，只有连接、等待响应头与流空闲三个约束。
3. **`ChatStream` 实现 `Stream`**，也可以用 `recv()`；丢弃即取消并关闭连接。
4. **`StreamEvent` / `LlmError` / `ClientConfig` 等标记了 `#[non_exhaustive]`**，下游 `match` 需保留 `_` 分支（后续新增变体不构成破坏性变更）。
5. **默认不重试**（`max_retries = 0`），压测与延迟统计优先；需要容错时显式开启。
6. **拿不到的数据返回 `None` 而不是 0**，避免把「无数据」当成「速度为 0」参与汇总。
7. **API Key 不会出现在 `Debug` 输出里**（形如 `sk-***`），可以安全地打日志。
8. **响应体有上限**（默认 32 MiB），防止异常服务端或代理返回超大内容撑爆内存。

## License

[Apache License 2.0](../../../LICENSE)
