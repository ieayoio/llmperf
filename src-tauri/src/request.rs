use futures::StreamExt;
use reqwest::Client as ReqwestClient;
use serde_json::json;
use tauri::Emitter;

use crate::types::{LLMRequestConfig, SingleResult, StreamChunkEvent};

/// 创建 reqwest HTTP 客户端
pub fn build_http_client() -> ReqwestClient {
    ReqwestClient::builder()
        .build()
        .expect("构建 HTTP 客户端失败")
}

/// 构建流式请求的 JSON body
pub fn build_request_body(config: &LLMRequestConfig) -> serde_json::Value {
    // 将前端传入的消息转换为 JSON 数组
    let messages: Vec<serde_json::Value> = config.messages.iter().map(|m| {
        json!({
            "role": m.role,
            "content": m.content,
        })
    }).collect();

    // 构造完整的请求体
    json!({
        "model": config.model.trim(),
        "messages": messages,
        "stream": true,
    })
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

/// 执行单次流式请求，返回最终结果
///
/// 该函数通过 reqwest 直接发送 HTTP 请求，手动构造 JSON body：
/// 1. 构造包含完整对话历史的请求体
/// 2. 逐块接收 SSE 流式响应并推送给前端
/// 3. 返回聚合的完整内容
pub async fn execute_stream_request(
    app: tauri::AppHandle,
    window_id: usize,
    config: LLMRequestConfig,
) -> SingleResult {
    let start = std::time::Instant::now();

    // 1. 构造请求体（手动构建 JSON，避免 async-openai builder 的序列化问题）
    let body = build_request_body(&config);
    let base_url = config.base_url.trim().to_string();
    let api_key = config.api_key.trim().to_string();

    // 2. 发送流式请求
    let client = build_http_client();
    let url = format!("{}/chat/completions", base_url);

    let response = match client
        .post(&url)
        .bearer_auth(&api_key)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            let error_msg = format!("请求发送失败: {e}");
            emit_error(&app, window_id, start, error_msg.clone());
            return SingleResult {
                window_id,
                assistant_content: String::new(),
                duration_ms: start.elapsed().as_millis(),
                error: Some(error_msg),
            };
        }
    };

    // 3. 检查 HTTP 状态码
    if !response.status().is_success() {
        let status = response.status();
        let error_body = response.text().await.unwrap_or_default();
        let error_msg = format!("HTTP {status}: {error_body}");
        emit_error(&app, window_id, start, error_msg.clone());
        return SingleResult {
            window_id,
            assistant_content: String::new(),
            duration_ms: start.elapsed().as_millis(),
            error: Some(error_msg),
        };
    }

    // 4. 逐块处理 SSE 流式响应
    let mut accumulated = String::new();
    let mut last_error: Option<String> = None;
    let mut stream = response.bytes_stream();

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(bytes) => {
                let chunk_str = String::from_utf8_lossy(&bytes).to_string();

                // 解析 SSE 格式的数据行 (data: {...})
                for line in chunk_str.split("\n") {
                    let line = line.trim();
                    if !line.starts_with("data: ") {
                        continue;
                    }
                    let data = &line[6..]; // 去掉 "data: " 前缀

                    // 跳过 [DONE] 结束标记
                    if data.trim() == "[DONE]" {
                        return SingleResult {
                            window_id,
                            assistant_content: accumulated,
                            duration_ms: start.elapsed().as_millis(),
                            error: None,
                        };
                    }

                    // 解析 JSON chunk
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                        // 提取当前 chunk 的内容
                        if let Some(content) = parsed
                            .get("choices")
                            .and_then(|c| c.get(0))
                            .and_then(|c| c.get("delta"))
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            if !content.is_empty() {
                                accumulated.push_str(content);
                                emit_chunk(&app, window_id, start, content);
                            }
                        }
                    }
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
        duration_ms: start.elapsed().as_millis(),
        error,
    }
}
