use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::chat::{
    ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessage,
    ChatCompletionRequestUserMessage,
    CreateChatCompletionRequestArgs,
    CreateChatCompletionStreamResponse,
};
use futures::StreamExt;
use tauri::Emitter;

use crate::types::{LLMRequestConfig, SingleResult, StreamChunkEvent};

/// 从配置创建 OpenAI 客户端
pub fn build_client(config: &LLMRequestConfig) -> Client<OpenAIConfig> {
    Client::with_config(
        OpenAIConfig::new()
            .with_api_key(config.api_key.trim())
            .with_api_base(config.base_url.trim()),
    )
}

/// 构建聊天完成请求
pub fn build_request(config: &LLMRequestConfig) -> Result<
    async_openai::types::chat::CreateChatCompletionRequest,
    async_openai::error::OpenAIError,
> {
    // 构造消息列表：系统提示 + 用户消息
    let messages: Vec<ChatCompletionRequestMessage> = vec![
        ChatCompletionRequestSystemMessage::from("You are a helpful assistant.").into(),
        ChatCompletionRequestUserMessage::from(config.message.trim().to_string()).into(),
    ];

    CreateChatCompletionRequestArgs::default()
        .model(config.model.trim())
        .messages(messages)
        .build()
}

/// 发送错误事件到前端
fn emit_error(
    app: &tauri::AppHandle,
    window_id: usize,
    start: std::time::Instant,
    error: String,
) {
    let _ = app.emit("stream_chunk", StreamChunkEvent {
        window_id,
        content: String::new(),
        finished: true,
        error: Some(error),
        duration_ms: start.elapsed().as_millis(),
    });
}

/// 发送成功完成事件到前端
fn emit_finished(
    app: &tauri::AppHandle,
    window_id: usize,
    start: std::time::Instant,
    _content: &str,
) {
    let _ = app.emit("stream_chunk", StreamChunkEvent {
        window_id,
        content: String::new(),
        finished: true,
        error: None,
        duration_ms: start.elapsed().as_millis(),
    });
}

/// 发送流式 chunk 到前端
fn emit_chunk(
    app: &tauri::AppHandle,
    window_id: usize,
    start: std::time::Instant,
    chunk: &str,
) {
    let _ = app.emit("stream_chunk", StreamChunkEvent {
        window_id,
        content: chunk.to_string(),
        finished: false,
        error: None,
        duration_ms: start.elapsed().as_millis(),
    });
}

/// 检查流式响应是否结束
fn is_stream_finished(response: &CreateChatCompletionStreamResponse) -> bool {
    response.choices.first()
        .map(|c| c.finish_reason.is_some())
        .unwrap_or(false)
}

/// 执行单次流式请求，返回最终结果
///
/// 该函数处理完整的请求生命周期：
/// 1. 构建客户端和请求
/// 2. 逐块接收响应并推送给前端
/// 3. 返回聚合的完整内容
pub async fn execute_stream_request(
    app: tauri::AppHandle,
    window_id: usize,
    config: LLMRequestConfig,
) -> SingleResult {
    let start = std::time::Instant::now();

    // 1. 创建客户端
    let client = build_client(&config);

    // 2. 构建请求
    let request = match build_request(&config) {
        Ok(r) => r,
        Err(e) => {
            let error_msg = format!("构建请求失败: {e}");
            emit_error(&app, window_id, start, error_msg.clone());
            return SingleResult {
                window_id,
                assistant_content: String::new(),
                duration_ms: start.elapsed().as_millis(),
                error: Some(error_msg),
            };
        }
    };

    // 3. 发送流式请求
    let mut stream = match client.chat().create_stream(request).await {
        Ok(s) => s,
        Err(e) => {
            let error_msg = format!("流式请求失败: {e}");
            emit_error(&app, window_id, start, error_msg);
            return SingleResult {
                window_id,
                assistant_content: String::new(),
                duration_ms: start.elapsed().as_millis(),
                error: Some("流式请求失败".to_string()),
            };
        }
    };

    // 4. 逐块处理并推送
    let mut accumulated = String::new();
    let mut last_error: Option<String> = None;

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(response) => {
                // 提取当前 chunk 的内容
                let content = response
                    .choices
                    .first()
                    .and_then(|c| c.delta.content.clone())
                    .unwrap_or_default();

                if !content.is_empty() {
                    accumulated.push_str(&content);
                    emit_chunk(&app, window_id, start, &content);
                }

                // 检查是否结束
                if is_stream_finished(&response) {
                    break;
                }
            }
            Err(e) => {
                last_error = Some(format!("流式响应错误: {e}"));
                emit_error(
                    &app,
                    window_id,
                    start,
                    last_error.clone().unwrap_or_default(),
                );
                break;
            }
        }
    }

    // 5. 发送完成事件
    let final_duration = start.elapsed().as_millis();
    emit_finished(&app, window_id, start, &accumulated);

    // 如果有错误但流已自然结束，标记错误
    let error = if last_error.is_some() {
        last_error
    } else {
        None
    };

    SingleResult {
        window_id,
        assistant_content: accumulated,
        duration_ms: final_duration,
        error,
    }
}
