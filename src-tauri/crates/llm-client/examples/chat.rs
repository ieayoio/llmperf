//! 非流式调用示例
//!
//! ```bash
//! LLM_BASE_URL="http://127.0.0.1:16777/v1" \
//! LLM_API_KEY="sk-xxx" LLM_MODEL="testllm-nk" \
//!   cargo run -p llm-client --example chat
//! ```

use llm_client::{ChatMessage, ChatParams, ClientConfig, LlmClient};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_url =
        std::env::var("LLM_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
    let api_key = std::env::var("LLM_API_KEY").unwrap_or_default();
    let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

    let client = LlmClient::new(ClientConfig::new(base_url, api_key))?;
    let params = ChatParams::new(model)
        .with_temperature(0.7)
        .with_max_tokens(256);

    let messages = vec![
        ChatMessage::system("你是一个简洁高效的助手，请用中文回答。"),
        ChatMessage::user("用一句话介绍 Rust 语言。"),
    ];

    let completion = client.chat(messages, &params).await?;

    if let Some(reasoning) = completion.reasoning_content() {
        println!("[思考]\n{reasoning}\n");
    }
    println!("{}", completion.content());
    println!("finish_reason: {:?}", completion.finish_reason());
    if let Some(usage) = &completion.usage {
        println!(
            "tokens: prompt={} completion={} total={}",
            usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
        );
    }
    if let Some(tps) = completion.completion_tokens_per_second() {
        println!("服务端 timings 速度: {tps:.2} tok/s");
    }

    Ok(())
}
