//! 流式调用示例：实时打印思考与正文，结束后输出压测指标
//!
//! ```bash
//! LLM_BASE_URL="http://127.0.0.1:16777/v1" \
//! LLM_API_KEY="sk-xxx" LLM_MODEL="testllm" \
//!   cargo run -p llm-client --example stream
//! ```

use std::time::Instant;

use llm_client::{
    ChatMessage, ChatParams, ClientConfig, LlmClient, StreamAccumulator, StreamEvent, StreamTiming,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_url =
        std::env::var("LLM_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
    let api_key = std::env::var("LLM_API_KEY").unwrap_or_default();
    let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

    let client = LlmClient::new(ClientConfig::new(base_url, api_key))?;
    let params = ChatParams::new(model).with_max_tokens(256);

    let start = Instant::now();
    let mut stream = client
        .chat_stream(
            vec![ChatMessage::user("用三句话解释什么是机器学习。")],
            &params,
        )
        .await?;

    let mut accumulator = StreamAccumulator::default();
    let mut timing = StreamTiming::new(start);

    while let Some(event) = stream.recv().await {
        let event = event?;
        timing.observe(&event);
        accumulator.feed(&event);
        match &event {
            StreamEvent::ReasoningDelta { text, .. } => print!("[思考]{text}"),
            StreamEvent::ContentDelta { text, .. } => print!("{text}"),
            StreamEvent::ToolCallDelta { .. } => {}
            StreamEvent::Finish(info) => {
                println!("\n[结束] reason={:?} done={}", info.reason, info.done)
            }
            // 枚举标记了 non_exhaustive，保留通配分支
            _ => {}
        }
    }

    let (completion_tps, prompt_tps) =
        timing.tokens_per_second(accumulator.timings.as_ref(), accumulator.usage.as_ref());
    let (tpot_ms, itl) = timing.itl();

    println!("\n--- 指标 ---");
    println!("总耗时: {} ms", timing.total_ms());
    println!("首 token: {:?} ms", timing.first_token_ms);
    println!("首字延迟: {:?} ms", timing.first_content_ms);
    println!("TPOT: {tpot_ms:?} ms/分片");
    println!("ITL: {itl:?}");
    println!("补全速度: {completion_tps:?} tok/s");
    println!("prefill 速度: {prompt_tps:?} tok/s");
    println!("是否完整: {}", accumulator.is_complete());

    Ok(())
}
