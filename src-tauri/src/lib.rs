use async_openai::{
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessage,
        ChatCompletionRequestUserMessage,
        CreateChatCompletionRequestArgs,
    },
    Client,
};
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// 单个窗口的请求结果
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SingleResult {
    pub window_id: usize,
    pub assistant_content: String,
    pub duration_ms: u128,
    pub error: Option<String>,
}

/// 前端传入的配置
#[derive(Deserialize, Debug, Clone)]
pub struct LLMRequestConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub message: String,
}

// ⚠️ lib.rs 中的命令不能加 pub，否则 Tauri 2 会报重复定义错误
#[tauri::command]
async fn send_concurrent_request(
    config: LLMRequestConfig,
    concurrency: usize,
) -> Vec<SingleResult> {
    let base_url = config.base_url.trim().to_string();
    let api_key = config.api_key.trim().to_string();
    let model = config.model.trim().to_string();
    let user_message = config.message.trim().to_string();

    let mut handles = Vec::new();

    for window_id in 0..concurrency {
        let base_url = base_url.to_string();
        let api_key = api_key.clone();
        let model = model.clone();
        let user_message = user_message.clone();

        handles.push(tauri::async_runtime::spawn(async move {
            let start = Instant::now();

            // Client::with_config 直接返回 Client，不是 Result
            let client = Client::with_config(
                OpenAIConfig::new()
                    .with_api_key(api_key.as_str())
                    .with_api_base(&base_url),
            );

            let messages: Vec<ChatCompletionRequestMessage> = vec![
                ChatCompletionRequestSystemMessage::from("You are a helpful assistant.")
                    .into(),
                ChatCompletionRequestUserMessage::from(user_message.clone())
                    .into(),
            ];

            let request = match CreateChatCompletionRequestArgs::default()
                .model(model.as_str())
                .messages(messages)
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    return SingleResult {
                        window_id,
                        assistant_content: String::new(),
                        duration_ms: start.elapsed().as_millis(),
                        error: Some(format!("构建请求失败: {e}")),
                    };
                }
            };

            match client.chat().create(request).await {
                Ok(response) => {
                    let content = response
                        .choices
                        .first()
                        .and_then(|c| c.message.content.clone())
                        .unwrap_or_default();

                    SingleResult {
                        window_id,
                        assistant_content: content,
                        duration_ms: start.elapsed().as_millis(),
                        error: None,
                    }
                }
                Err(e) => SingleResult {
                    window_id,
                    assistant_content: String::new(),
                    duration_ms: start.elapsed().as_millis(),
                    error: Some(format!("{e}")),
                },
            }
        }));
    }

    let mut results = Vec::with_capacity(concurrency);
    for handle in handles {
        results.push(handle.await.unwrap());
    }

    results
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![send_concurrent_request])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
