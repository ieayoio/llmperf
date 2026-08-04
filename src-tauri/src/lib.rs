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
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tauri::Emitter;

/// 单个窗口的最终请求结果
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SingleResult {
    pub window_id: usize,
    pub assistant_content: String,
    pub duration_ms: u128,
    pub error: Option<String>,
}

/// 流式 chunk 事件
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StreamChunkEvent {
    pub window_id: usize,
    pub content: String,
    pub finished: bool,
    pub error: Option<String>,
    pub duration_ms: u128,
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
    app: tauri::AppHandle,
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
        let app = app.clone();

        handles.push(tauri::async_runtime::spawn(async move {
            let start = Instant::now();

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
                    let _ = app.emit("stream_chunk", StreamChunkEvent {
                        window_id,
                        content: String::new(),
                        finished: true,
                        error: Some(format!("构建请求失败: {e}")),
                        duration_ms: start.elapsed().as_millis(),
                    });
                    return SingleResult {
                        window_id,
                        assistant_content: String::new(),
                        duration_ms: start.elapsed().as_millis(),
                        error: Some(format!("构建请求失败: {e}")),
                    };
                }
            };

            let mut accumulated = String::new();

            // 使用流式 API
            let mut stream = match client.chat().create_stream(request).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = app.emit("stream_chunk", StreamChunkEvent {
                        window_id,
                        content: String::new(),
                        finished: true,
                        error: Some(format!("流式请求失败: {e}")),
                        duration_ms: start.elapsed().as_millis(),
                    });
                    return SingleResult {
                        window_id,
                        assistant_content: String::new(),
                        duration_ms: start.elapsed().as_millis(),
                        error: Some(format!("流式请求失败: {e}")),
                    };
                }
            };

            let last_error: Option<String>;
            // 逐 chunk 处理并推送
            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(response) => {
                        let content = response
                            .choices
                            .first()
                            .and_then(|c| c.delta.content.clone())
                            .unwrap_or_default();

                        if !content.is_empty() {
                            accumulated.push_str(&content);
                            let _ = app.emit("stream_chunk", StreamChunkEvent {
                                window_id,
                                content: content.clone(),
                                finished: false,
                                error: None,
                                duration_ms: start.elapsed().as_millis(),
                            });
                        }

                        // 检查是否结束
                        if response.choices.first()
                            .map(|c| c.finish_reason.is_some())
                            .unwrap_or(false)
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        last_error = Some(format!("流式响应错误: {e}"));
                        let _ = app.emit("stream_chunk", StreamChunkEvent {
                            window_id,
                            content: String::new(),
                            finished: true,
                            error: last_error.clone(),
                            duration_ms: start.elapsed().as_millis(),
                        });
                        break;
                    }
                }
            }

            // 发送最终完成事件
            let final_duration = start.elapsed().as_millis();
            let _ = app.emit("stream_chunk", StreamChunkEvent {
                window_id,
                content: String::new(),
                finished: true,
                error: None,
                duration_ms: final_duration,
            });

            SingleResult {
                window_id,
                assistant_content: accumulated,
                duration_ms: final_duration,
                error: None,
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
