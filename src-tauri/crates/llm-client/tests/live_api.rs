//! 真实 API 集成测试
//!
//! 配置通过环境变量注入，**不要在源码中硬编码任何凭据**。
//!
//! ```bash
//! export LLMPERF_TEST_BASE_URL="http://127.0.0.1:16777/v1"
//! export LLMPERF_TEST_API_KEY="sk-xxxxxxxx"
//! export LLMPERF_TEST_MODEL_CHAT="testllm-nk"
//! export LLMPERF_TEST_MODEL_REASONER="testllm"
//!
//! cargo test -p llm-client --test live_api
//! ```
//!
//! 任一必需环境变量缺失时，测试会跳过（不 fail），以便在 CI 等无 API 环境下保持绿灯。

use std::time::Instant;

use llm_client::{
    ChatMessage, ChatParams, ClientConfig, LlmClient, LlmError, StreamAccumulator, StreamEvent,
    StreamTiming,
};

/// 返回全部必需环境变量；任一缺失则整个测试文件 skip
fn test_config() -> Option<(String, String, String, String)> {
    let base_url = std::env::var("LLMPERF_TEST_BASE_URL")
        .ok()?
        .trim()
        .to_string();
    let api_key = std::env::var("LLMPERF_TEST_API_KEY")
        .ok()?
        .trim()
        .to_string();
    let chat = std::env::var("LLMPERF_TEST_MODEL_CHAT")
        .ok()?
        .trim()
        .to_string();
    let reasoner = std::env::var("LLMPERF_TEST_MODEL_REASONER")
        .ok()?
        .trim()
        .to_string();
    if base_url.is_empty() || api_key.is_empty() || chat.is_empty() || reasoner.is_empty() {
        return None;
    }
    Some((base_url, api_key, chat, reasoner))
}

fn build_test_client() -> Option<LlmClient> {
    let (base_url, api_key, _, _) = test_config()?;
    LlmClient::new(
        ClientConfig::new(base_url, api_key)
            .with_response_timeout(std::time::Duration::from_secs(120))
            .with_stream_idle_timeout(Some(std::time::Duration::from_secs(120)))
            .with_total_timeout(Some(std::time::Duration::from_secs(120))),
    )
    .ok()
}

fn build_test_messages() -> Vec<ChatMessage> {
    vec![
        ChatMessage::system("你是一个简洁高效的 AI 助手。请用中文回答。"),
        ChatMessage::user("请用一句话解释什么是机器学习？"),
    ]
}

fn chat_model() -> Option<String> {
    test_config().map(|(_, _, chat, _)| chat)
}

fn reasoner_model() -> Option<String> {
    test_config().map(|(_, _, _, reasoner)| reasoner)
}

// -------- 非流式调用 --------

/// 非流式调用：验证能正常获取模型回复与 token 统计
#[tokio::test]
async fn test_chat_non_stream() {
    let Some(client) = build_test_client() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_* 环境变量");
        return;
    };
    let Some(model) = chat_model() else {
        return;
    };

    let params = ChatParams::new(model)
        .with_temperature(0.7)
        .with_max_tokens(512);
    let completion = client
        .chat(build_test_messages(), &params)
        .await
        .expect("非流式调用失败");

    if let Some(reasoning) = completion.reasoning_content() {
        println!("💭 思考内容:\n{reasoning}\n");
    }
    println!("📝 回复内容:\n{}", completion.content());
    if let Some(usage) = &completion.usage {
        println!(
            "📊 Token 统计：prompt={}, completion={}, total={}",
            usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
        );
    }
    assert!(!completion.content().is_empty(), "回复内容不应为空");
}

/// 思考模型（非流式）：`reasoning_content` 或 `reasoning` 字段至少能取到
#[tokio::test]
async fn test_reasoning_model_non_stream() {
    let Some(client) = build_test_client() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_* 环境变量");
        return;
    };
    let Some(model) = reasoner_model() else {
        return;
    };

    let params = ChatParams::new(model)
        .with_temperature(0.6)
        .with_max_tokens(1024);
    let messages = vec![
        ChatMessage::system("你是一个推理能力强的 AI 助手。"),
        ChatMessage::user("为什么天空是蓝色的？请用简短的推理过程回答。"),
    ];
    let completion = client
        .chat(messages, &params)
        .await
        .expect("思考模型调用失败");

    match completion.reasoning_content() {
        Some(reasoning) => {
            println!("💭 思考过程:\n{reasoning}\n");
            assert!(!reasoning.is_empty());
        }
        None => println!("⚠️ 该模型未返回思考内容，可能不是推理模型"),
    }
    assert!(!completion.content().is_empty(), "回复内容不应为空");
}

// -------- 流式调用 --------

/// 流式调用：逐 token 事件、累加结果与首 token 打点
#[tokio::test]
async fn test_chat_stream() {
    let Some(client) = build_test_client() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_* 环境变量");
        return;
    };
    let Some(model) = chat_model() else {
        return;
    };

    let params = ChatParams::new(model)
        .with_temperature(0.7)
        .with_max_tokens(512);
    let mut stream = client
        .chat_stream(build_test_messages(), &params)
        .await
        .expect("流式调用初始化失败");

    let mut accumulator = StreamAccumulator::default();
    let mut reasoning_delta_count = 0u32;
    let mut content_delta_count = 0u32;

    while let Some(event) = stream.recv().await {
        let event = event.expect("流式事件解析失败");
        match &event {
            StreamEvent::ReasoningDelta { .. } => reasoning_delta_count += 1,
            StreamEvent::ContentDelta { .. } => content_delta_count += 1,
            _ => {}
        }
        accumulator.feed(&event);
    }

    println!("💭 思考内容:\n{}", accumulator.reasoning_content());
    println!("📝 回复内容:\n{}", accumulator.content());
    println!("📈 事件数：reasoning={reasoning_delta_count}, content={content_delta_count}");

    assert!(!accumulator.content().is_empty(), "流式正文不应为空");
    assert!(accumulator.is_complete(), "应收到结束标记");
    assert!(accumulator.first_token_at.is_some(), "首 token 应被打点");
}

/// 思考模型（流式）：思考增量必须先于正文出现
#[tokio::test]
async fn test_reasoning_model_stream() {
    let Some(client) = build_test_client() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_* 环境变量");
        return;
    };
    let Some(model) = reasoner_model() else {
        return;
    };

    let params = ChatParams::new(model)
        .with_temperature(0.6)
        .with_max_tokens(1024);
    let messages = vec![
        ChatMessage::system("你是一个推理能力强的 AI 助手。"),
        ChatMessage::user("为什么天空是蓝色的？"),
    ];
    let mut stream = client
        .chat_stream(messages, &params)
        .await
        .expect("思考模型流式调用初始化失败");

    let mut accumulator = StreamAccumulator::default();
    let mut first_event_is_reasoning = false;
    let mut first = true;

    while let Some(event) = stream.recv().await {
        let event = event.expect("流式事件解析失败");
        if first {
            first_event_is_reasoning = matches!(event, StreamEvent::ReasoningDelta { .. });
            first = false;
        }
        accumulator.feed(&event);
    }

    println!("💭 思考内容:\n{}", accumulator.reasoning_content());
    println!("📝 回复内容:\n{}", accumulator.content());
    assert!(
        !accumulator.content().is_empty() || !accumulator.reasoning_content().is_empty(),
        "至少应有一路内容"
    );
    if !accumulator.reasoning_content().is_empty() {
        assert!(first_event_is_reasoning, "首个增量应是思考内容");
    }
}

// -------- 参数与多轮 --------

/// 扩展参数（response_format）透传
#[tokio::test]
async fn test_chat_with_extra_params() {
    let Some(client) = build_test_client() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_* 环境变量");
        return;
    };
    let Some(model) = chat_model() else {
        return;
    };

    let params =
        ChatParams::new(model).with_extra("response_format", serde_json::json!({ "type": "text" }));
    let completion = client
        .chat(vec![ChatMessage::user("回答是或否：1+1=2 对吗？")], &params)
        .await
        .expect("带扩展参数的调用失败");
    assert!(!completion.content().is_empty());
}

/// 多轮对话：assistant 历史消息正常透传
#[tokio::test]
async fn test_chat_multi_turn() {
    let Some(client) = build_test_client() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_* 环境变量");
        return;
    };
    let Some(model) = chat_model() else {
        return;
    };

    let params = ChatParams::new(model)
        .with_temperature(0.7)
        .with_max_tokens(256);
    let messages = vec![
        ChatMessage::system("你是一个问答助手，回答要简洁。"),
        ChatMessage::user("中国的首都是哪里？"),
        ChatMessage::assistant("北京。"),
        ChatMessage::user("那它的常住人口大约是多少？"),
    ];
    let completion = client
        .chat(messages, &params)
        .await
        .expect("多轮对话调用失败");
    assert!(!completion.content().is_empty());
}

// -------- 错误处理 --------

/// 无效 API Key：应拿到 401 与结构化错误码
#[tokio::test]
async fn test_invalid_api_key_error() {
    let Some((base_url, _, _, _)) = test_config() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_* 环境变量");
        return;
    };
    let Some(model) = chat_model() else {
        return;
    };
    let client =
        LlmClient::new(ClientConfig::new(base_url, "sk-invalid-key-test")).expect("客户端创建失败");

    let result = client
        .chat(
            vec![ChatMessage::user("测试无效 key")],
            &ChatParams::new(model),
        )
        .await;
    match result {
        Err(LlmError::Api { status, .. }) => assert_eq!(status, 401, "应返回 401"),
        other => panic!("预期 API 错误，但得到：{other:?}"),
    }
}

/// 不可达地址：应为 HTTP 层错误而不是 panic
#[tokio::test]
async fn test_invalid_url_error() {
    let client = LlmClient::new(ClientConfig::new("http://192.0.2.1:9999/v1", "sk-any"))
        .expect("客户端创建失败");
    let result = client
        .chat(
            vec![ChatMessage::user("测试网络错误")],
            &ChatParams::new("any"),
        )
        .await;
    assert!(matches!(result, Err(LlmError::Http(_))), "应为 HTTP 错误");
}

// -------- 指标口径联调 --------

/// 客户端分片间隔应与服务端 timings 反推的每 token 耗时处于同一量级
#[tokio::test]
async fn live_itl_matches_server_timings() {
    let Some(client) = build_test_client() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_* 环境变量");
        return;
    };
    let Some(model) = chat_model() else {
        return;
    };

    let params = ChatParams::new(model).with_max_tokens(48);
    let start = Instant::now();
    let mut stream = client
        .chat_stream(vec![ChatMessage::user("用一句话介绍你自己")], &params)
        .await
        .expect("流式请求应能发出");

    let mut accumulator = StreamAccumulator::default();
    let mut timing = StreamTiming::new(start);
    while let Some(event) = stream.recv().await {
        let event = event.expect("流式事件不应报错");
        timing.observe(&event);
        accumulator.feed(&event);
    }

    assert!(
        timing.delta_samples().len() >= 5,
        "样本太少，无法验证间隔统计"
    );
    let (tpot, itl) = timing.itl();
    let tpot = tpot.expect("应能算出平均分片间隔");
    assert!(itl.is_some(), "应输出 ITL 分位数");
    assert!(tpot > 0.0, "平均间隔必须为正数");

    // 服务端 timings 是权威值，两边口径不同（客户端含网络抖动），只校验量级
    if let Some(per_token_ms) = accumulator
        .timings
        .as_ref()
        .and_then(|t| t.completion_tokens_per_second())
        .map(|tps| 1000.0 / tps)
    {
        let ratio = tpot / per_token_ms;
        assert!(
            (0.5..2.0).contains(&ratio),
            "客户端 TPOT {tpot:.1}ms 与服务端 {per_token_ms:.1}ms 偏差过大（比值 {ratio:.2}）"
        );
    }
}
