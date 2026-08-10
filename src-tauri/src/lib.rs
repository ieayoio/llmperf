// Tauri 2 中 lib.rs 的命令不能加 pub，否则报重复定义错误
mod types;
mod request;
mod concurrent;
mod llm_tool;
mod menu;

use tauri::{Emitter, Manager, Runtime};

pub use types::{SingleResult, StreamChunkEvent, LLMRequestConfig};
pub use llm_tool::{
    ChatCompletion, ChatMessage, ChatParams, ClientConfig, LlmClient, LlmError, Role,
    StreamAccumulator, StreamEvent, Timings,
};
use menu::LangItems;

/// Tauri 命令：设置初始语言（用于同步菜单栏勾选状态）
///
/// 前端在初始化时调用此命令，将 localStorage 中保存的语言设置同步到后端菜单。
#[tauri::command]
fn set_initial_language(
    app: tauri::AppHandle,
    lang: String,
) {
    // 从 app state 获取语言菜单项并更新勾选状态
    let lang_items = app.state::<LangItems<tauri::Wry>>();
    let is_zh_sc = lang == "zh-SC";
    let is_zh_tc = lang == "zh-TC";
    let _ = lang_items.zh_sc.set_checked(is_zh_sc);
    let _ = lang_items.zh_tc.set_checked(is_zh_tc);
    let _ = lang_items.en.set_checked(!is_zh_sc && !is_zh_tc);

    // 重建菜单以同步文本
    rebuild_menu(&app, &lang);

    // 通知所有前端窗口菜单已同步
    for window in app.webview_windows().values() {
        let _ = window.emit("language-sync", &lang);
    }
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
        Ok((new_menu, new_zh_sc, new_zh_tc, new_en)) => {
            // 应用新菜单
            if let Err(e) = app.set_menu(new_menu) {
                eprintln!("[menu] 设置新菜单失败: {}", e);
                return;
            }

            // 将新的语言菜单项注册到 app state
            app.manage(crate::menu::LangItems {
                zh_sc: new_zh_sc.clone(),
                zh_tc: new_zh_tc.clone(),
                en: new_en.clone(),
            });
        }
        Err(e) => {
            eprintln!("[menu] 创建新菜单失败: {}", e);
        }
    }
}

/// 对所有 webview 窗口执行同一操作
fn for_each_window<R: Runtime, F>(app: &tauri::AppHandle<R>, mut f: F)
where
    F: FnMut(&tauri::WebviewWindow<R>),
{
    for window in app.webview_windows().values() {
        f(window);
    }
}

/// 切换应用语言：重建菜单并通知所有前端窗口
fn switch_lang<R: Runtime>(app: &tauri::AppHandle<R>, lang: &str) {
    rebuild_menu(app, lang);
    for_each_window(app, |w| {
        let _ = w.emit("language-changed", lang);
    });
}

/// 退出应用
fn quit_app<R: Runtime>(app: &tauri::AppHandle<R>) {
    app.exit(0);
}

/// 最小化所有窗口
fn minimize_all_windows<R: Runtime>(app: &tauri::AppHandle<R>) {
    for_each_window(app, |w| {
        let _ = w.minimize();
    });
}

/// 最大化或还原所有窗口
fn toggle_maximize_all_windows<R: Runtime>(app: &tauri::AppHandle<R>) {
    for_each_window(app, |w| {
        if w.is_maximized().unwrap_or(false) {
            let _ = w.unmaximize();
        } else {
            let _ = w.maximize();
        }
    });
}

/// 关闭所有窗口
fn close_all_windows<R: Runtime>(app: &tauri::AppHandle<R>) {
    for_each_window(app, |w| {
        let _ = w.close();
    });
}

/// 显示关于对话框（通过事件通知前端）
fn show_about<R: Runtime>(app: &tauri::AppHandle<R>) {
    for_each_window(app, |w| {
        let _ = w.emit("menu-about", &());
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![send_concurrent_request, set_initial_language])
        // 创建原生菜单栏 + 菜单事件处理
        .setup(|app| {
            // 创建菜单，同时获取语言菜单项的 Arc 引用
            // 初始语言默认为简体中文，后续会通过 set_initial_language 命令更新
            let (menu, lang_zh_sc, lang_zh_tc, lang_en) = menu::create_menu(app.handle(), "zh-SC")?;
            app.set_menu(menu)?;

            // 将语言菜单项注册到 app state，供命令访问
            app.manage(menu::LangItems {
                zh_sc: lang_zh_sc.clone(),
                zh_tc: lang_zh_tc.clone(),
                en: lang_en.clone(),
            });

            // 注册菜单事件处理器：处理语言切换 + 窗口操作
            app.on_menu_event(move |app, event| {
                let id = event.id();

                match id.as_ref() {
                    // === 语言切换 ===
                    "lang-zh-SC" => switch_lang(app, "zh-SC"),
                    "lang-zh-TC" => switch_lang(app, "zh-TC"),
                    "lang-en" => switch_lang(app, "en"),

                    // === 应用菜单 ===
                    "about" => show_about(app),
                    "quit" => quit_app(app),

                    // === 窗口操作 ===
                    "win-minimize" => minimize_all_windows(app),
                    "win-maximize" => toggle_maximize_all_windows(app),
                    "win-close" => close_all_windows(app),

                    // === macOS 专属 ===
                    #[cfg(target_os = "macos")]
                    "hide" => {
                        for_each_window(app, |w| {
                            let _ = w.hide();
                        });
                    }
                    #[cfg(target_os = "macos")]
                    "hide-others" => {
                        let _ = app.hide_other();
                    }

                    // 未知 ID：忽略
                    _ => {}
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
