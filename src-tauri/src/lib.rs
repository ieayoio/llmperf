// Tauri 2 中 lib.rs 的命令不能加 pub，否则报重复定义错误
mod types;
mod request;
mod concurrent;
mod cancel;
mod llm_tool;
mod menu;

use tauri::{Emitter, Manager, Runtime};

pub use types::{SingleResult, StreamChunkEvent, LLMRequestConfig};
pub use llm_tool::{
    ChatCompletion, ChatMessage, ChatParams, ClientConfig, LlmClient, LlmError, Role,
    StreamAccumulator, StreamEvent, Timings,
};

/// Tauri 命令：设置初始语言（用于把 localStorage 中的语言同步到菜单栏）
///
/// 前端在初始化时调用此命令，将上次选择的语言同步到后端原生菜单。
#[tauri::command]
fn set_initial_language(
    app: tauri::AppHandle,
    lang: String,
) {
    apply_menu_language(&app, &lang);

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

/// Tauri 命令：取消指定窗口的请求
///
/// 调用后，对应 `window_id` 的流式循环会立即退出，并向前端发出 `finished=true`
/// 且带 `error="用户已取消"` 的 `stream_chunk` 事件，前端据此更新窗口状态。
///
/// 返回 `true` 表示该窗口存在并触发了取消；`false` 表示该窗口没有活跃请求。
#[tauri::command]
async fn cancel_request(app: tauri::AppHandle, window_id: usize) -> bool {
    app.state::<cancel::CancelRegistry>().cancel(window_id).await
}

/// Tauri 命令：一次性取消所有正在执行的请求
///
/// 返回被取消的 `window_id` 列表，便于前端调试或统计。
#[tauri::command]
async fn cancel_all_requests(app: tauri::AppHandle) -> Vec<usize> {
    app.state::<cancel::CancelRegistry>().cancel_all().await
}

/// 校验导出文件名：只允许纯文件名
///
/// 拒绝空串、路径分隔符与上跳片段，防止被利用做任意路径写入。
fn is_safe_export_filename(filename: &str) -> bool {
    !filename.is_empty()
        && !filename.contains('/')
        && !filename.contains('\\')
        && !filename.contains("..")
}

/// Tauri 命令：把文本内容写入系统下载目录，返回完整路径
///
/// 用于导出测试结果（CSV）。仅接受纯文件名：包含路径分隔符或上跳片段的
/// 名字一律拒绝，避免被利用做任意路径写入。
#[tauri::command]
fn save_text_file(
    app: tauri::AppHandle,
    filename: String,
    content: String,
) -> Result<String, String> {
    if !is_safe_export_filename(&filename) {
        return Err("文件名不合法".to_string());
    }

    let dir = app
        .path()
        .download_dir()
        .map_err(|e| format!("无法定位下载目录: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建下载目录失败: {e}"))?;

    let path = dir.join(&filename);
    std::fs::write(&path, content).map_err(|e| format!("写入文件失败: {e}"))?;
    Ok(path.display().to_string())
}

/// 就地更新原生菜单的语言
///
/// 不重建菜单：`AppHandle::set_menu()` 内部会先 `remove_menu()`，再异步挂载新菜单，
/// 中间会有一帧菜单栏不存在，webview 被迫拉伸到整个窗口高度再回缩，
/// 表现为切换语言时界面抖动。菜单项本身支持就地改文本，因此只需更新文本。
fn apply_menu_language<R: Runtime>(app: &tauri::AppHandle<R>, lang: &str) {
    app.state::<menu::MenuHandle<R>>().apply_language(lang);
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

/// 切换应用语言：更新菜单文本并通知所有前端窗口
fn switch_lang<R: Runtime>(app: &tauri::AppHandle<R>, lang: &str) {
    apply_menu_language(app, lang);
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
        // 注册取消注册表（按 window_id 管理 CancellationToken）
        .manage(cancel::CancelRegistry::new())
        .invoke_handler(tauri::generate_handler![
            send_concurrent_request,
            set_initial_language,
            cancel_request,
            cancel_all_requests,
            save_text_file,
        ])
        // 创建原生菜单栏 + 菜单事件处理
        .setup(|app| {
            // 创建菜单（初始语言默认简体中文，随后由 set_initial_language 按用户设置就地更新）
            let (menu, menu_handle) = menu::create_menu(app.handle(), "zh-SC")?;
            app.set_menu(menu)?;

            // 菜单句柄只注册一次：切换语言时就地改文本，不重建菜单
            app.manage(menu_handle);

            // 注册菜单事件处理器：处理语言切换 + 窗口操作
            app.on_menu_event(move |app, event| {
                let id = event.id();

                match id.as_ref() {
                    // === 语言切换：ID 约定为 `lang-<语言代码>`，与 menu.rs 保持一致 ===
                    "lang-zh-SC" => switch_lang(app, "zh-SC"),
                    "lang-zh-TC" => switch_lang(app, "zh-TC"),
                    "lang-en" => switch_lang(app, "en"),
                    "lang-ja" => switch_lang(app, "ja"),
                    "lang-ko" => switch_lang(app, "ko"),

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

#[cfg(test)]
mod tests {
    use super::*;

    /// 导出文件名只允许纯文件名，任何路径片段都必须被拒绝
    #[test]
    fn test_export_filename_validation() {
        assert!(is_safe_export_filename("llmperf-20261024-153000.csv"));
        assert!(!is_safe_export_filename(""));
        assert!(!is_safe_export_filename("../evil.csv"));
        assert!(!is_safe_export_filename("a/b.csv"));
        assert!(!is_safe_export_filename("a\\b.csv"));
    }
}
