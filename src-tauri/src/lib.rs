// Tauri 2 中 lib.rs 的命令不能加 pub，否则报重复定义错误
mod types;
mod request;
mod concurrent;
mod cancel;
mod llm_tool;
mod menu;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::{Emitter, Manager, Runtime};
use tauri_plugin_opener::OpenerExt;

pub use types::{SingleResult, StreamChunkEvent, LLMRequestConfig};
pub use llm_tool::{
    ChatCompletion, ChatMessage, ChatParams, ClientConfig, LlmClient, LlmError, Role,
    StreamAccumulator, StreamEvent, Timings,
};

/// 帮助菜单使用的固定链接
///
/// 写成编译期常量而不是从前端接收 URL，避免被利用打开任意链接。
const URL_HOMEPAGE: &str = "https://github.com/ieayoio/llmperf";
const URL_DOCS: &str = "https://github.com/ieayoio/llmperf/blob/main/README.md";

/// 缩放步进与上下限（百分比）
const ZOOM_STEP: i32 = 10;
const ZOOM_MIN: u16 = 50;
const ZOOM_MAX: u16 = 200;
const ZOOM_DEFAULT: u16 = 100;

/// 当前缩放比例（百分比）
///
/// 多个 webview 共用同一档位，因此单独存一份状态，
/// 避免每次点「放大」都要从窗口读取（平台支持程度不一致）。
struct ZoomState(Mutex<u16>);

impl ZoomState {
    fn new() -> Self {
        Self(Mutex::new(ZOOM_DEFAULT))
    }
}

/// 最近一次由前端同步过来的界面状态
///
/// 模式项是可勾选菜单项，点击时系统会自动翻转勾选；
/// 若点的恰好是当前模式，前端状态不会变化，这时就要用这份快照把勾选纠正回来。
struct LatestMenuState(Mutex<Option<menu::MenuState>>);

impl LatestMenuState {
    fn new() -> Self {
        Self(Mutex::new(None))
    }
}

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

/// Tauri 命令：把界面状态同步到原生菜单
///
/// 前端在模式、发送状态、结果数量变化时推送，菜单据此启用/禁用与勾选，
/// 避免出现「点了没反应」或「能点到不该点的项」。
#[tauri::command]
fn set_menu_state(app: tauri::AppHandle, state: menu::MenuState) {
    app.state::<menu::MenuHandle<tauri::Wry>>().apply_state(&state);

    // 留一份快照，供菜单点击后纠正自动翻转的勾选（见 resync_menu_state）
    if let Ok(mut latest) = app.state::<LatestMenuState>().0.lock() {
        *latest = Some(state);
    }
}

/// Tauri 命令：取「快捷键一览」数据
///
/// 快捷键表的唯一来源在 menu.rs，前端只负责渲染，避免两边各写一份导致不一致。
#[tauri::command]
fn get_menu_shortcuts() -> Vec<menu::Shortcut> {
    menu::shortcuts()
}

/// 校验导出路径：必须是绝对路径且指向文件
///
/// 路径由系统保存对话框返回（用户可自选目录与文件名），这里只做兜底校验：
/// 拒绝相对路径（会落到当前工作目录）与已存在的目录（避免把目录当文件写）。
fn validate_export_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("保存路径必须是绝对路径".to_string());
    }
    if path.is_dir() {
        return Err("保存路径不能是目录".to_string());
    }
    if path.file_name().and_then(|name| name.to_str()).is_none() {
        return Err("保存路径缺少文件名".to_string());
    }
    Ok(())
}

/// Tauri 命令：把文本内容写入用户选定的路径，返回完整路径
///
/// 用于导出测试结果（CSV）。路径由前端调用系统保存对话框得到，
/// 用户可在弹窗中修改目录与文件名；本命令只负责校验与落盘。
#[tauri::command]
fn save_text_file(path: String, content: String) -> Result<String, String> {
    let path = PathBuf::from(path);
    validate_export_path(&path)?;

    // 保存对话框一般已创建好目录，这里兼容用户手动输入的新目录
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
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

/// 切换所有窗口的全屏状态
///
/// 以任一窗口当前状态取反作为目标状态，保证多窗口表现一致。
fn toggle_fullscreen_all_windows<R: Runtime>(app: &tauri::AppHandle<R>) {
    let target = !app
        .webview_windows()
        .values()
        .next()
        .and_then(|w| w.is_fullscreen().ok())
        .unwrap_or(false);
    for_each_window(app, |w| {
        let _ = w.set_fullscreen(target);
    });
}

/// 把百分比缩放应用到所有窗口
fn apply_zoom<R: Runtime>(app: &tauri::AppHandle<R>, percent: u16) {
    for_each_window(app, |w| {
        // set_zoom 接收倍数，1.0 表示 100%；不支持的平台忽略失败
        let _ = w.set_zoom(f64::from(percent) / 100.0);
    });
}

/// 在当前缩放下加减一档，并限制在 [ZOOM_MIN, ZOOM_MAX]
fn zoom_step<R: Runtime>(app: &tauri::AppHandle<R>, delta: i32) {
    let state = app.state::<ZoomState>();
    let next = {
        // 锁被毒化时放弃本次缩放，不影响其他功能
        let Ok(mut percent) = state.0.lock() else {
            return;
        };
        *percent = (*percent as i32 + delta).clamp(ZOOM_MIN as i32, ZOOM_MAX as i32) as u16;
        *percent
    };
    apply_zoom(app, next);
}

/// 缩放恢复到 100%
fn zoom_reset<R: Runtime>(app: &tauri::AppHandle<R>) {
    let state = app.state::<ZoomState>();
    if let Ok(mut percent) = state.0.lock() {
        *percent = ZOOM_DEFAULT;
    }
    apply_zoom(app, ZOOM_DEFAULT);
}

/// 重新加载界面（仅 debug 构建提供，release 下会丢失测试结果）
#[cfg(debug_assertions)]
fn reload_all_windows<R: Runtime>(app: &tauri::AppHandle<R>) {
    for_each_window(app, |w| {
        let _ = w.reload();
    });
}

/// 打开开发者工具（仅 debug 构建提供）
#[cfg(debug_assertions)]
fn open_devtools<R: Runtime>(app: &tauri::AppHandle<R>) {
    for_each_window(app, |w| w.open_devtools());
}

/// 用系统默认程序打开外部链接（失败只记录日志，不打断应用）
fn open_url<R: Runtime>(app: &tauri::AppHandle<R>, url: &str) {
    if let Err(e) = app.opener().open_url(url, None::<&str>) {
        eprintln!("[llmperf] 打开链接失败 {url}: {e}");
    }
}

/// 打开系统下载目录（导出文件默认落在这里）
fn open_download_dir<R: Runtime>(app: &tauri::AppHandle<R>) {
    match app.path().download_dir() {
        Ok(dir) => {
            // 目录不存在时先创建，避免打开失败
            let _ = std::fs::create_dir_all(&dir);
            if let Err(e) = app.opener().open_path(dir.display().to_string(), None::<&str>) {
                eprintln!("[llmperf] 打开下载目录失败: {e}");
            }
        }
        Err(e) => eprintln!("[llmperf] 无法定位下载目录: {e}"),
    }
}

/// 把动作转发给前端执行
///
/// 发送、取消、切模式、导出等动作的最终状态都在 React 里，
/// 后端只负责把菜单点击统一转成 `menu-command` 事件。
fn forward_to_frontend<R: Runtime>(app: &tauri::AppHandle<R>, command: &str) {
    for_each_window(app, |w| {
        let _ = w.emit("menu-command", command);
    });
}

/// 用最近一次界面状态重刷菜单
///
/// 可勾选的模式项被点击时，系统会无条件翻转勾选；
/// 若点的就是当前模式，前端不会重推状态，菜单就会多出一个错误的对勾。
fn resync_menu_state<R: Runtime>(app: &tauri::AppHandle<R>) {
    let holder = app.state::<LatestMenuState>();
    let latest = match holder.0.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => None,
    };
    if let Some(state) = latest {
        app.state::<menu::MenuHandle<R>>().apply_state(&state);
    }
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
        // 打开链接/目录插件：只在 Rust 侧调用，关闭对 <a target=_blank> 的 JS 注入
        // （前端不使用该插件的 JS API，因此无需额外的 capability 权限）
        .plugin(tauri_plugin_opener::Builder::new().open_js_links_on_click(false).build())
        // 保存对话框插件：导出 CSV 时由用户在弹窗中确认目录与文件名
        .plugin(tauri_plugin_dialog::init())
        // 注册取消注册表（按 window_id 管理 CancellationToken）
        .manage(cancel::CancelRegistry::new())
        .invoke_handler(tauri::generate_handler![
            send_concurrent_request,
            set_initial_language,
            cancel_request,
            cancel_all_requests,
            save_text_file,
            set_menu_state,
            get_menu_shortcuts,
        ])
        // 创建原生菜单栏 + 菜单事件处理
        .setup(|app| {
            // 创建菜单（初始语言默认简体中文，随后由 set_initial_language 按用户设置就地更新）
            let (menu, menu_handle) = menu::create_menu(app.handle(), "zh-SC")?;
            app.set_menu(menu)?;

            // 菜单句柄只注册一次：切换语言时就地改文本，不重建菜单
            app.manage(menu_handle);
            // 缩放比例：所有窗口共享同一档位
            app.manage(ZoomState::new());
            // 最近一次界面状态：用于菜单点击后纠正勾选
            app.manage(LatestMenuState::new());

            // 注册菜单事件处理器：后端直接能做的就地执行，其余转发给前端
            app.on_menu_event(move |app, event| {
                let id = event.id();

                // 语言项：ID 约定为 `lang-<语言代码>`，新增语言无需改这里
                if let Some(code) = id.as_ref().strip_prefix("lang-") {
                    switch_lang(app, code);
                    return;
                }

                match id.as_ref() {
                    // === 后端直接执行的动作 ===
                    "about" => show_about(app),
                    "quit" => quit_app(app),
                    "win-minimize" => minimize_all_windows(app),
                    "win-maximize" => toggle_maximize_all_windows(app),
                    "win-close" => close_all_windows(app),
                    "view-fullscreen" => toggle_fullscreen_all_windows(app),
                    "view-zoom-in" => zoom_step(app, ZOOM_STEP),
                    "view-zoom-out" => zoom_step(app, -ZOOM_STEP),
                    "view-zoom-reset" => zoom_reset(app),
                    "file-open-downloads" => open_download_dir(app),
                    "help-homepage" => open_url(app, URL_HOMEPAGE),
                    "help-docs" => open_url(app, URL_DOCS),
                    #[cfg(debug_assertions)]
                    "view-reload" => reload_all_windows(app),
                    #[cfg(debug_assertions)]
                    "view-devtools" => open_devtools(app),

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

                    // === 其余动作交给前端（发送、取消、切模式、导出等） ===
                    other => {
                        // 模式项的勾选先按当前真实状态纠正，避免出现两个对勾或没有对勾
                        if other.starts_with("run-mode-") {
                            resync_menu_state(app);
                        }
                        forward_to_frontend(app, other);
                    }
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

    /// 导出路径校验：相对路径、目录、缺少文件名都必须拒绝
    #[test]
    fn test_export_path_validation() {
        assert!(validate_export_path(Path::new("/tmp/llmperf-20261024-153000.csv")).is_ok());
        assert!(validate_export_path(Path::new("llmperf-20261024-153000.csv")).is_err());
        assert!(validate_export_path(Path::new("/tmp")).is_err());
        assert!(validate_export_path(Path::new("/")).is_err());
    }
}
