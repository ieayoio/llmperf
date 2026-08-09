// Tauri 2 中 lib.rs 的命令不能加 pub，否则报重复定义错误
mod types;
mod request;
mod concurrent;
mod llm_tool;
mod menu;

use std::sync::Arc;

use tauri::{Emitter, Manager, Runtime};

pub use types::{SingleResult, StreamChunkEvent, LLMRequestConfig};
pub use llm_tool::{LlmClient, ClientConfig, ChatMessage, ChatParams, ChatCompletion, Role, StreamEvent, Timings};

/// Tauri 命令：设置初始语言（用于同步菜单栏勾选状态）
///
/// 前端在初始化时调用此命令，将 localStorage 中保存的语言设置同步到后端菜单。
#[tauri::command]
fn set_initial_language(
    app: tauri::AppHandle,
    lang: String,
) {
    // 从 app state 获取语言菜单项并更新勾选状态
    let lang_items = app.state::<crate::menu::LangItems<tauri::Wry>>();
    let is_zh = lang == "zh";
    let _ = lang_items.zh.set_checked(is_zh);
    let _ = lang_items.en.set_checked(!is_zh);

    // 重建菜单以同步文本
    rebuild_menu(&app, &lang);

    // 通知所有前端窗口菜单已同步
    for window in app.webview_windows().values() {
        let _ = window.emit("language-sync", &lang);
    }
}

/// 存储语言菜单项的容器，用于在命令中访问和更新
pub struct LangItems<R: Runtime> {
    pub zh: Arc<tauri::menu::CheckMenuItem<R>>,
    pub en: Arc<tauri::menu::CheckMenuItem<R>>,
}

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

/// 重建菜单栏：移除旧菜单并创建新菜单
///
/// 在 Linux/GTK 上，直接修改菜单项文本不一定能触发 UI 刷新，
/// 因此采用重建整个菜单的方式来确保文本正确更新。
fn rebuild_menu<R: Runtime>(app: &tauri::AppHandle<R>, lang: &str) {
    // 移除旧菜单（会同时移除所有窗口上的旧菜单）
    let _ = app.remove_menu();

    // 创建新菜单，create_menu 会根据 lang 参数正确设置勾选状态
    match menu::create_menu(app, lang) {
        Ok((new_menu, new_zh, new_en)) => {
            // 应用新菜单
            if let Err(e) = app.set_menu(new_menu) {
                eprintln!("[menu] 设置新菜单失败: {}", e);
                return;
            }

            // 将新的语言菜单项注册到 app state
            app.manage(crate::menu::LangItems {
                zh: new_zh.item.clone(),
                en: new_en.item.clone(),
            });
        }
        Err(e) => {
            eprintln!("[menu] 创建新菜单失败: {}", e);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![send_concurrent_request, set_initial_language])
        // 创建原生菜单栏 + 菜单事件处理
        .setup(|app| {
            // 创建菜单，同时获取语言菜单项的 Arc 引用
            // 初始语言默认为中文，后续会通过 set_initial_language 命令更新
            let (menu, lang_zh, lang_en) = menu::create_menu(app.handle(), "zh")?;
            app.set_menu(menu)?;

            // 将语言菜单项注册到 app state，供命令访问
            app.manage(menu::LangItems {
                zh: lang_zh.item.clone(),
                en: lang_en.item.clone(),
            });

            // 注册菜单事件处理器：处理语言切换 + 窗口操作
            app.on_menu_event(move |app, event| {
                let id = event.id();

                // === 语言切换 ===
                if *id == "lang-zh" {
                    // 重建菜单以同步文本
                    rebuild_menu(&app, "zh");
                    // 通知所有前端窗口切换语言
                    for window in app.webview_windows().values() {
                        let _ = window.emit("language-changed", "zh");
                    }
                } else if *id == "lang-en" {
                    // 重建菜单以同步文本
                    rebuild_menu(&app, "en");
                    // 通知所有前端窗口切换语言
                    for window in app.webview_windows().values() {
                        let _ = window.emit("language-changed", "en");
                    }
                }
                // === 应用菜单 ===
                if *id == "about" {
                    // 显示关于对话框（通过事件通知前端）
                    for window in app.webview_windows().values() {
                        let _ = window.emit("menu-about", &());
                    }
                } else if *id == "quit" {
                    // 退出应用
                    app.exit(0);
                } else {
                    #[cfg(target_os = "macos")]
                    if *id == "hide" {
                        // macOS: 隐藏当前应用的所有窗口
                        for window in app.webview_windows().values() {
                            let _ = window.hide();
                        }
                    } else if *id == "hide-others" {
                        // macOS: 隐藏其他应用
                        let _ = app.hide_other();
                    } else {
                        // === 窗口操作 ===
                        if *id == "win-minimize" {
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
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
