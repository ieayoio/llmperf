// Tauri 2 中 lib.rs 的命令不能加 pub，否则报重复定义错误
mod types;
mod request;
mod concurrent;
mod llm_tool;
mod menu;

use tauri::{Emitter, Manager};

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
        // 创建原生菜单栏 + 菜单事件处理
        .setup(|app| {
            let menu = menu::create_menu(app.handle())?;
            app.set_menu(menu)?;

            // 注册菜单事件处理器：处理语言切换
            // 注意：必须通过 WebviewWindow::emit 发送事件，
            // 因为前端 listen 监听的是窗口级事件，而不是应用级事件。
            app.on_menu_event(|app, event| {
                let id = event.id();
                if *id == "lang-zh" {
                    // 遍历所有窗口，向每个窗口发送事件
                    for window in app.webview_windows().values() {
                        let _ = window.emit("language-changed", "zh");
                    }
                } else if *id == "lang-en" {
                    for window in app.webview_windows().values() {
                        let _ = window.emit("language-changed", "en");
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
