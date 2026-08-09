/// Tauri 原生菜单栏定义
///
/// 使用 Tauri 2.11 的原生菜单系统，提供带勾选提示的语言切换功能。
/// 点击菜单项时会触发 menu_event，前端通过监听事件来切换语言。
/// 菜单文本会随语言切换通过重建菜单来动态更新。

use std::sync::Arc;
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    AppHandle, Runtime,
};

/// 语言菜单项引用（用于更新勾选状态）
pub struct LangItem<R: Runtime> {
    /// 菜单项引用（用于更新勾选状态）
    pub item: Arc<CheckMenuItem<R>>,
}

/// 存储语言菜单项的容器，用于在命令中访问和更新
pub struct LangItems<R: Runtime> {
    /// 中文菜单项
    pub zh: Arc<CheckMenuItem<R>>,
    /// 英文菜单项
    pub en: Arc<CheckMenuItem<R>>,
}

/// 创建应用的原生菜单栏
///
/// 当前包含以下菜单：
/// - **应用**：关于、分隔线、退出
/// - **语言**：中文（带勾选）、English（带勾选）
/// - **窗口**：最小化、最大化/还原、关闭
///
/// 勾选状态会随语言切换自动更新，菜单文本也会同步更新。
///
/// # 参数
/// * `initial_lang` - 初始化语言（"zh" 或 "en"），用于设置菜单项的初始勾选状态和文本
///
/// # 返回值
/// * `(Menu, LangItem, LangItem)` - 主菜单、中文菜单项、英文菜单项
pub fn create_menu<R: Runtime>(app: &AppHandle<R>, initial_lang: &str) -> tauri::Result<(Menu<R>, LangItem<R>, LangItem<R>)> {
    let is_zh = initial_lang == "zh";

    // 根据语言确定菜单文本
    let app_name = if is_zh { "应用" } else { "App" };
    let lang_name = if is_zh { "语言" } else { "Language" };
    let window_name = if is_zh { "窗口" } else { "Window" };
    let about_text = if is_zh { "关于 LLMPerf..." } else { "About LLMPerf..." };
    let quit_text = if is_zh { "退出" } else { "Quit" };
    let minimize_text = if is_zh { "最小化" } else { "Minimize" };
    let maximize_text = if is_zh { "最大化" } else { "Maximize" };
    let close_text = if is_zh { "关闭窗口" } else { "Close Window" };
    #[cfg(target_os = "macos")]
    let hide_text = if is_zh { "隐藏" } else { "Hide" };
    #[cfg(target_os = "macos")]
    let hide_others_text = if is_zh { "隐藏其他" } else { "Hide Others" };

    // === 应用菜单（使用自定义 MenuItem，以便文本可更新） ===
    let about_item = MenuItem::with_id(app, "about".to_string(), about_text, true, None::<String>)?;
    let quit_item = MenuItem::with_id(app, "quit".to_string(), quit_text, true, None::<String>)?;
    #[cfg(target_os = "macos")]
    let hide_item = MenuItem::with_id(app, "hide".to_string(), hide_text, true, None::<String>)?;
    #[cfg(target_os = "macos")]
    let hide_others_item = MenuItem::with_id(app, "hide-others".to_string(), hide_others_text, true, None::<String>)?;

    let app_menu = Submenu::with_items(
        app,
        app_name,
        true,
        &[
            &about_item,
            &PredefinedMenuItem::separator(app)?,
            #[cfg(target_os = "macos")]
            &hide_item,
            #[cfg(target_os = "macos")]
            &hide_others_item,
            #[cfg(target_os = "macos")]
            &PredefinedMenuItem::separator(app)?,
            &quit_item,
        ],
    )?;

    // === 语言菜单：使用 CheckMenuItem 实现勾选提示 ===
    let lang_zh_item = CheckMenuItem::with_id(
        app,
        "lang-zh".to_string(),
        "中文",
        true,
        is_zh,
        None::<String>,
    )?;
    let lang_zh = Arc::new(lang_zh_item);

    let lang_en_item = CheckMenuItem::with_id(
        app,
        "lang-en".to_string(),
        "English",
        true,
        !is_zh,
        None::<String>,
    )?;
    let lang_en = Arc::new(lang_en_item);

    let language_menu = Submenu::with_items(
        app,
        lang_name,
        true,
        &[&*lang_zh, &*lang_en],
    )?;

    // === 窗口菜单 ===
    let win_minimize = MenuItem::with_id(
        app, "win-minimize".to_string(), minimize_text, true, None::<String>,
    )?;
    let win_maximize = MenuItem::with_id(
        app, "win-maximize".to_string(), maximize_text, true, None::<String>,
    )?;
    let win_close = MenuItem::with_id(
        app, "win-close".to_string(), close_text, true, None::<String>,
    )?;

    let window_menu = Submenu::with_items(
        app,
        window_name,
        true,
        &[
            &win_minimize,
            &win_maximize,
            &PredefinedMenuItem::separator(app)?,
            &win_close,
        ],
    )?;

    let menu = Menu::with_items(
        app,
        &[&app_menu, &language_menu, &window_menu],
    )?;

    let lang_zh_config = LangItem { item: lang_zh };
    let lang_en_config = LangItem { item: lang_en };

    Ok((menu, lang_zh_config, lang_en_config))
}
