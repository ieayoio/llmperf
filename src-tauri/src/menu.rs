/// Tauri 原生菜单栏定义
///
/// 使用 Tauri 2.11 的原生菜单系统，提供语言切换功能。
/// 点击菜单项时会触发 menu_event，前端通过监听事件来切换语言。

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    AppHandle, Runtime,
};

/// 创建应用的原生菜单栏
///
/// 当前包含以下菜单：
/// - **应用**：关于、分隔线、退出
/// - **语言**：中文、English
/// - **窗口**：最小化
///
/// 如需添加新语言，只需在 `语言` 子菜单中添加新的 `MenuItem`，
/// 并在前端 `i18n/index.ts` 的 `SUPPORTED_LANGUAGES` 中注册。
pub fn create_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    // === 应用菜单（macOS 上自动成为菜单栏，Windows/Linux 上作为第一个菜单）===
    let app_menu = Submenu::with_items(
        app,
        "应用",
        true,
        &[
            &PredefinedMenuItem::about(
                app,
                None,
                Some(tauri::menu::AboutMetadata {
                    name: Some("LLMPerf".into()),
                    ..Default::default()
                }),
            )?,
            &PredefinedMenuItem::separator(app)?,
            #[cfg(target_os = "macos")]
            &PredefinedMenuItem::hide(app, None)?,
            #[cfg(target_os = "macos")]
            &PredefinedMenuItem::hide_others(app, None)?,
            #[cfg(target_os = "macos")]
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;

    // === 语言菜单：支持中文和英文切换 ===
    // MenuItem::with_id 用于给菜单项设置唯一 ID，
    // 后端通过匹配 event.id 来识别用户点击了哪个语言选项
    let lang_zh = MenuItem::with_id(app, "lang-zh".to_string(), "中文", true, None::<String>)?;
    let lang_en = MenuItem::with_id(app, "lang-en".to_string(), "English", true, None::<String>)?;

    let language_menu = Submenu::with_items(app, "语言", true, &[&lang_zh, &lang_en])?;

    // === 窗口菜单 ===
    let window_menu = Submenu::with_items(
        app,
        "窗口",
        true,
        &[&PredefinedMenuItem::minimize(app, None)?],
    )?;

    // 构建主菜单
    let menu = Menu::with_items(app, &[&app_menu, &language_menu, &window_menu])?;

    Ok(menu)
}
