// Tauri 2 中 lib.rs 的命令不能加 pub，否则报重复定义错误
mod types;
mod request;
mod concurrent;
mod llm_tool;

pub use types::{SingleResult, StreamChunkEvent, LLMRequestConfig};
pub use llm_tool::{LlmClient, ClientConfig, ChatMessage, ChatParams, ChatCompletion, Role, StreamEvent};

/// Tauri 命令：并发发送 LLM 请求
///
/// 接收前端传入的配置和并发数，启动多个并发任务，
/// 每个任务独立处理流式响应并推送给前端，最终汇总结果返回。
#[tauri::command]
async fn send_concurrent_request(
    app: tauri::AppHandle,
    config: LLMRequestConfig,
    concurrency: usize,
) -> Vec<SingleResult> {
    concurrent::run_concurrent_requests(app, config, concurrency).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![send_concurrent_request])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
