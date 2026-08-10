//! # LLM 工具类集成测试
//!
//! 本文件包含需要真实 API 配置的集成测试。配置通过环境变量注入，
//! **不要在源码中硬编码任何凭据**。
//!
//! ## 运行方式
//!
//! ```bash
//! # 设置环境变量
//! export LLMPERF_TEST_BASE_URL="http://127.0.0.1:16777/v1"
//! export LLMPERF_TEST_API_KEY="sk-xxxxxxxx"
//! export LLMPERF_TEST_MODEL_CHAT="testllm-nk"
//! export LLMPERF_TEST_MODEL_REASONER="testllm"
//!
//! # 运行所有集成测试
//! cargo test --test llm_tool_integration
//!
//! # 运行单个测试
//! cargo test --test llm_tool_integration -- test_chat_non_stream
//!
//! # 运行思考模型相关测试
//! cargo test --test llm_tool_integration -- reasoning
//! ```
//!
//! 若任一必需环境变量缺失，该测试文件下的所有测试会被自动跳过（不 fail），
//! 以便在 CI 等无 API 环境下保持绿灯。

use llmperf_lib::*;

/// 返回全部必需环境变量；任一缺失则整个测试文件 skip
fn test_config() -> Option<(String, String, String, String)> {
    let base_url = std::env::var("LLMPERF_TEST_BASE_URL").ok()?.trim().to_string();
    let api_key = std::env::var("LLMPERF_TEST_API_KEY").ok()?.trim().to_string();
    let chat = std::env::var("LLMPERF_TEST_MODEL_CHAT").ok()?.trim().to_string();
    let reasoner = std::env::var("LLMPERF_TEST_MODEL_REASONER").ok()?.trim().to_string();
    if base_url.is_empty() || api_key.is_empty() || chat.is_empty() || reasoner.is_empty() {
        return None;
    }
    Some((base_url, api_key, chat, reasoner))
}

fn build_test_client() -> Option<LlmClient> {
    let (base_url, api_key, _, _) = test_config()?;
    let config = ClientConfig {
        base_url,
        api_key,
        timeout_secs: 120,
    };
    LlmClient::new(config).ok()
}

fn build_test_messages() -> Vec<ChatMessage> {
    vec![
        ChatMessage::system("你是一个简洁高效的 AI 助手。请用中文回答。"),
        ChatMessage::user("请用一句话解释什么是机器学习？"),
    ]
}

fn chat_model() -> Option<String> {
    test_config().map(|(_, _, c, _)| c)
}

fn reasoner_model() -> Option<String> {
    test_config().map(|(_, _, _, r)| r)
}

// -------- 非流式调用 --------

/// 非流式调用测试：验证能正常获取模型回复和思考内容
#[tokio::test]
async fn test_chat_non_stream() {
    let Some(client) = build_test_client() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_BASE_URL / LLMPERF_TEST_API_KEY / LLMPERF_TEST_MODEL_CHAT 环境变量");
        return;
    };
    let Some(model) = chat_model() else {
        return;
    };
    let params = ChatParams {
        model,
        temperature: Some(0.7),
        max_tokens: Some(512),
        ..Default::default()
    };
    let messages = build_test_messages();

    println!("\n========== 非流式调用测试 ==========");

    let completion = client.chat(messages, &params).await.expect("非流式调用失败");

    // 打印结果
    if let Some(reasoning) = completion.reasoning_content() {
        println!("💭 思考内容:\n{reasoning}\n");
    }
    println!("📝 回复内容:\n{}", completion.content());
    if let Some(finish_reason) = completion.finish_reason() {
        println!("🔚 结束原因：{finish_reason}");
    }
    if let Some(usage) = &completion.usage {
        println!(
            "📊 Token 统计：prompt={}, completion={}, total={}",
            usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
        );
    }

    // 断言
    assert!(
        !completion.content().is_empty(),
        "回复内容不应为空"
    );
}

// -------- 流式调用 --------

/// 流式调用测试：验证 SSE 逐 token 推送和事件解析
#[tokio::test]
async fn test_chat_stream() {
    let Some(client) = build_test_client() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_BASE_URL / LLMPERF_TEST_API_KEY / LLMPERF_TEST_MODEL_CHAT 环境变量");
        return;
    };
    let Some(model) = chat_model() else {
        return;
    };
    let params = ChatParams {
        model,
        temperature: Some(0.7),
        max_tokens: Some(512),
        ..Default::default()
    };
    let messages = build_test_messages();

    println!("\n========== 流式调用测试 ==========");

    let mut rx = client
        .chat_stream(messages, &params)
        .await
        .expect("流式调用初始化失败");

    let mut accumulator = StreamAccumulator::default();
    let mut reasoning_delta_count = 0u32;
    let mut content_delta_count = 0u32;

    while let Some(event_result) = rx.recv().await {
        let event = event_result.expect("流式事件解析失败");
        match &event {
            StreamEvent::ReasoningDelta(s) => {
                reasoning_delta_count += 1;
                print!("💭{s}");
            }
            StreamEvent::ContentDelta(s) => {
                content_delta_count += 1;
                print!("{s}");
            }
            StreamEvent::Finish(info) => {
                println!("\n✅ 生成完成，原因：{}", info.reason);
                if let Some(usage) = &info.usage {
                    println!(
                        "📊 Token 统计：prompt={}, completion={}, total={}",
                        usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                    );
                }
            }
        }
        accumulator.feed(&event);
    }

    // 断言
    assert!(
        !accumulator.content.is_empty(),
        "流式累积的正文内容不应为空"
    );
    println!(
        "\n📈 统计：reasoning_delta 事件={reasoning_delta_count}, content_delta 事件={content_delta_count}"
    );
}

// -------- 思考模型测试 --------

/// 思考模型测试：验证 reasoning_content 字段的流式/非流式解析
/// 使用 deepseek-reasoner 模型，该模型会输出思考过程
#[tokio::test]
async fn test_reasoning_model_non_stream() {
    let Some(client) = build_test_client() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_BASE_URL / LLMPERF_TEST_API_KEY / LLMPERF_TEST_MODEL_REASONER 环境变量");
        return;
    };
    let Some(model) = reasoner_model() else {
        return;
    };
    let params = ChatParams {
        model,
        temperature: Some(0.6),
        max_tokens: Some(1024),
        ..Default::default()
    };
    let messages = vec![
        ChatMessage::system("你是一个推理能力强的 AI 助手。"),
        ChatMessage::user("为什么天空是蓝色的？请用简短的推理过程回答。"),
    ];

    println!("\n========== 思考模型 (非流式) 测试 ==========");

    let completion = client.chat(messages, &params).await.expect("思考模型调用失败");

    // 思考模型应该返回 reasoning_content
    let reasoning = completion.reasoning_content();
    match reasoning {
        Some(r) => {
            println!("💭 思考过程:\n{r}\n");
            assert!(!r.is_empty(), "思考内容不应为空");
        }
        None => {
            println!("⚠️ 该模型未返回 reasoning_content，可能不是推理模型");
        }
    }

    println!("📝 回复内容:\n{}", completion.content());
    assert!(
        !completion.content().is_empty(),
        "回复内容不应为空"
    );
}

/// 思考模型流式测试：验证推理过程中的 ReasoningDelta 事件
#[tokio::test]
async fn test_reasoning_model_stream() {
    let Some(client) = build_test_client() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_BASE_URL / LLMPERF_TEST_API_KEY / LLMPERF_TEST_MODEL_REASONER 环境变量");
        return;
    };
    let Some(model) = reasoner_model() else {
        return;
    };
    let params = ChatParams {
        model,
        temperature: Some(0.6),
        max_tokens: Some(1024),
        ..Default::default()
    };
    let messages = vec![
        ChatMessage::system("你是一个推理能力强的 AI 助手。"),
        ChatMessage::user("为什么天空是蓝色的？"),
    ];

    println!("\n========== 思考模型 (流式) 测试 ==========");

    let mut rx = client
        .chat_stream(messages, &params)
        .await
        .expect("思考模型流式调用初始化失败");

    let mut accumulator = StreamAccumulator::default();
    let mut has_reasoning = false;

    while let Some(event_result) = rx.recv().await {
        let event = event_result.expect("流式事件解析失败");
        match &event {
            StreamEvent::ReasoningDelta(_) => {
                has_reasoning = true;
            }
            StreamEvent::ContentDelta(_) => {
                // 正文增量事件
            }
            StreamEvent::Finish(_) => {}
        }
        accumulator.feed(&event);
    }

    // 打印最终结果
    if has_reasoning {
        println!("💭 思考内容:\n{}", accumulator.reasoning_content);
    }
    println!("📝 回复内容:\n{}", accumulator.content);

    // 断言：至少有一个字段有内容
    assert!(
        accumulator.content.contains("蓝色")
            || accumulator.reasoning_content.contains("蓝色")
            || accumulator.content.contains("散射"),
        "回复内容应包含相关关键词"
    );
}

// -------- 自定义扩展参数测试 --------

/// 测试通过 extra 字段传入自定义参数（如 response_format）
#[tokio::test]
async fn test_chat_with_extra_params() {
    let Some(client) = build_test_client() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_BASE_URL / LLMPERF_TEST_API_KEY / LLMPERF_TEST_MODEL_CHAT 环境变量");
        return;
    };
    let Some(model) = chat_model() else {
        return;
    };
    let mut extra = serde_json::Map::new();
    extra.insert(
        "response_format".to_string(),
        serde_json::json!({ "type": "text" }),
    );

    let params = ChatParams {
        model,
        extra,
        ..Default::default()
    };
    let messages = vec![
        ChatMessage::user("回答是或否：1+1=2 对吗？"),
    ];

    println!("\n========== 自定义扩展参数测试 ==========");

    let completion = client
        .chat(messages, &params)
        .await
        .expect("带扩展参数的调用失败");

    println!("📝 回复内容:\n{}", completion.content());
    assert!(!completion.content().is_empty());
}

// -------- 多轮对话测试 --------

/// 多轮对话测试：验证上下文消息的传递
#[tokio::test]
async fn test_chat_multi_turn() {
    let Some(client) = build_test_client() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_BASE_URL / LLMPERF_TEST_API_KEY / LLMPERF_TEST_MODEL_CHAT 环境变量");
        return;
    };
    let Some(model) = chat_model() else {
        return;
    };
    let params = ChatParams {
        model,
        temperature: Some(0.7),
        max_tokens: Some(256),
        ..Default::default()
    };

    let messages = vec![
        ChatMessage::system("你是一个问答助手，回答要简洁。"),
        ChatMessage::user("中国的首都是哪里？"),
        ChatMessage::assistant("北京。"),
        ChatMessage::user("那它的常住人口大约是多少？"),
    ];

    println!("\n========== 多轮对话测试 ==========");

    let completion = client.chat(messages, &params).await.expect("多轮对话调用失败");

    println!("📝 回复内容:\n{}", completion.content());
    assert!(
        completion.content().contains("万")
            || completion.content().contains("亿")
            || completion.content().contains("人口"),
        "回复应包含人口相关信息"
    );
}

// -------- 错误处理测试 --------

/// 测试无效 API Key 时的错误处理
#[tokio::test]
async fn test_invalid_api_key_error() {
    let Some((base_url, _, _, model)) = test_config() else {
        eprintln!("[skip] 缺少 LLMPERF_TEST_BASE_URL / LLMPERF_TEST_API_KEY / LLMPERF_TEST_MODEL_CHAT 环境变量");
        return;
    };
    let config = ClientConfig {
        base_url,
        api_key: "sk-invalid-key-test".to_string(),
        timeout_secs: 30,
    };
    let client = LlmClient::new(config).expect("客户端创建失败");

    let params = ChatParams {
        model,
        ..Default::default()
    };
    let messages = vec![ChatMessage::user("测试无效 key")];

    println!("\n========== 无效 APIKey 错误处理测试 ==========");

    let result = client.chat(messages, &params).await;
    match result {
        Err(LlmError::Api { status, message }) => {
            println!("✅ 正确捕获到 API 错误：HTTP {status} - {message}");
            assert_eq!(status, 401, "应该返回 401 鉴权失败");
        }
        other => panic!("预期 API 错误，但得到：{other:?}"),
    }
}

// -------- 超时错误处理测试 --------

/// 测试无效 base_url 时的网络错误处理
#[tokio::test]
async fn test_invalid_url_error() {
    let config = ClientConfig {
        base_url: "http://192.0.2.1:9999/v1".to_string(), // RFC 5737 保留地址，必定不通
        api_key: "sk-any-key".to_string(),
        timeout_secs: 5,
    };
    let client = LlmClient::new(config).expect("客户端创建失败");

    let params = ChatParams {
        model: "any".to_string(),
        ..Default::default()
    };
    let messages = vec![ChatMessage::user("测试网络错误")];

    println!("\n========== 无效 URL 错误处理测试 ==========");

    let result = client.chat(messages, &params).await;
    match result {
        Err(LlmError::Http(_)) => {
            println!("✅ 正确捕获到 HTTP 网络错误");
        }
        other => panic!("预期 HTTP 错误，但得到：{other:?}"),
    }
}
