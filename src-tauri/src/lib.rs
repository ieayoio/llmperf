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
            // 创建菜单，同时获取语言菜单项的 Arc 引用
            let (menu, lang_zh, lang_en) = menu::create_menu(app.handle())?;
            app.set_menu(menu)?;

            // 将语言项的 Arc 引用克隆到闭包中，用于在菜单事件中更新勾选状态
            let zh_item = lang_zh.item.clone();
            let en_item = lang_en.item.clone();

            // 注册菜单事件处理器：处理语言切换 + 窗口操作 + 勾选状态更新
            app.on_menu_event(move |app, event| {
                let id = event.id();
                
                // === 语言切换 ===
                if *id == "lang-zh" {
                    // 选中中文，取消英文勾选
                    let _ = zh_item.set_checked(true);
                    let _ = en_item.set_checked(false);
                    // 通知所有前端窗口切换语言
                    for window in app.webview_windows().values() {
                        let _ = window.emit("language-changed", "zh");
                    }
                } else if *id == "lang-en" {
                    // 选中英文，取消中文勾选
                    let _ = en_item.set_checked(true);
                    let _ = zh_item.set_checked(false);
                    // 通知所有前端窗口切换语言
                    for window in app.webview_windows().values() {
                        let _ = window.emit("language-changed", "en");
                    }
                }
                // === 窗口操作 ===
                else if *id == "win-minimize" {
                    // 最小化所有窗口
                    for window in app.webview_windows().values() {
                        let _ = window.minimize();
                    }
                } else if *id == "win-maximize" {
                    // 最大化/还原所有窗口
                    for window in app.webview_windows().values() {
                        if window.is_maximized().unwrap_or(false) {
                            let _ = window.unmaximize();
                        } else {
                            let _ = window.maximize();
                        }
                    }
                } else if *id == "win-close" {
                    // 关闭所有窗口
                    for window in app.webview_windows().values() {
                        let _ = window.close();
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
