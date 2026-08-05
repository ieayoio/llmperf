use crate::types::{LLMRequestConfig, SingleResult};
use crate::request::execute_stream_request;

/// 并发执行多个 LLM 请求
///
/// 为每个 window_id 创建独立的请求任务，等待所有任务完成后返回结果列表。
pub async fn run_concurrent_requests(
    app: tauri::AppHandle,
    config: LLMRequestConfig,
    concurrency: usize,
) -> Vec<SingleResult> {
    let base_url = config.base_url.trim().to_string();
    let api_key = config.api_key.trim().to_string();
    let model = config.model.trim().to_string();
    let messages = config.messages.clone();

    // 构建请求配置，克隆给每个并发任务（保留对话历史）
    let configs: Vec<LLMRequestConfig> = (0..concurrency)
        .map(|_| LLMRequestConfig {
            base_url: base_url.clone(),
            api_key: api_key.clone(),
            model: model.clone(),
            messages: messages.clone(),
        })
        .collect();

    // 启动所有并发任务
    let handles: Vec<_> = configs
        .into_iter()
        .enumerate()
        .map(|(window_id, config)| {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                execute_stream_request(app, window_id, config).await
            })
        })
        .collect();

    // 等待所有任务完成
    let mut results = Vec::with_capacity(concurrency);
    for handle in handles {
        results.push(handle.await.unwrap());
    }

    results
}
