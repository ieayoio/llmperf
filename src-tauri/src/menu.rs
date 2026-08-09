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
    /// 简体中文菜单项
    pub zh_sc: Arc<CheckMenuItem<R>>,
    /// 繁体中文菜单项
    pub zh_tc: Arc<CheckMenuItem<R>>,
    /// 英文菜单项
    pub en: Arc<CheckMenuItem<R>>,
}

/// 根据语言代码返回对应的菜单文本
/// 返回: (app_name, about_text, quit_text, hide_text, hide_others_text,
///        minimize_text, maximize_text, close_text,
///        lang_zh_sc_text, lang_zh_tc_text, lang_name, window_name)
fn zh_menu_texts(lang: &str) -> (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
) {
    if lang == "zh-SC" {
        (
            "应用",
            "关于 LLMPerf...",
            "退出",
            "隐藏",
            "隐藏其他",
            "最小化",
            "最大化",
            "关闭窗口",
            "简体中文",
            "繁體中文",
            "语言",
            "窗口",
        )
    } else if lang == "zh-TC" {
        (
            "應用程式",
            "關於 LLMPerf...",
            "離開",
            "隱藏",
            "隱藏其他",
            "最小化",
            "最大化",
            "關閉視窗",
            "簡體中文",
            "繁體中文",
            "語言",
            "視窗",
        )
    } else {
        (
            "App",
            "About LLMPerf...",
            "Quit",
            "Hide",
            "Hide Others",
            "Minimize",
            "Maximize",
            "Close Window",
            "简体中文",
            "繁體中文",
            "Language",
            "Window",
        )
    }
}

/// 创建应用的原生菜单栏
///
/// 当前包含以下菜单：
/// - **应用**：关于、分隔线、退出
/// - **语言**：简体中文（带勾选）、繁體中文（带勾选）、English（带勾选）
/// - **窗口**：最小化、最大化/还原、关闭
///
/// 勾选状态会随语言切换自动更新，菜单文本也会同步更新。
///
/// # 参数
/// * `initial_lang` - 初始化语言（"zh-SC"、"zh-TC" 或 "en"），用于设置菜单项的初始勾选状态和文本
///
/// # 返回值
/// * `(Menu, LangItem, LangItem, LangItem)` - 主菜单、简体中文菜单项、繁体中文菜单项、英文菜单项
pub fn create_menu<R: Runtime>(
    app: &AppHandle<R>,
    initial_lang: &str,
) -> tauri::Result<(Menu<R>, LangItem<R>, LangItem<R>, LangItem<R>)> {
    let (
        app_name,
        about_text,
        quit_text,
        _hide_text,
        _hide_others_text,
        minimize_text,
        maximize_text,
        close_text,
        lang_zh_sc_text,
        lang_zh_tc_text,
        lang_name,
        window_name,
    ) = zh_menu_texts(initial_lang);
    let is_zh_sc = initial_lang == "zh-SC";
    let is_zh_tc = initial_lang == "zh-TC";

    // === 应用菜单（使用自定义 MenuItem，以便文本可更新） ===
    let about_item = MenuItem::with_id(app, "about".to_string(), about_text, true, None::<String>)?;
    let quit_item = MenuItem::with_id(app, "quit".to_string(), quit_text, true, None::<String>)?;
    #[cfg(target_os = "macos")]
    let hide_item = MenuItem::with_id(app, "hide".to_string(), hide_text, true, None::<String>)?;
    #[cfg(target_os = "macos")]
    let hide_others_item = MenuItem::with_id(
        app,
        "hide-others".to_string(),
        hide_others_text,
        true,
        None::<String>,
    )?;

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
    let lang_zh_sc_item = CheckMenuItem::with_id(
        app,
        "lang-zh-SC".to_string(),
        lang_zh_sc_text,
        true,
        is_zh_sc,
        None::<String>,
    )?;
    let lang_zh_sc = Arc::new(lang_zh_sc_item);

    let lang_zh_tc_item = CheckMenuItem::with_id(
        app,
        "lang-zh-TC".to_string(),
        lang_zh_tc_text,
        true,
        is_zh_tc,
        None::<String>,
    )?;
    let lang_zh_tc = Arc::new(lang_zh_tc_item);

    let lang_en_item = CheckMenuItem::with_id(
        app,
        "lang-en".to_string(),
        "English",
        true,
        !is_zh_sc && !is_zh_tc,
        None::<String>,
    )?;
    let lang_en = Arc::new(lang_en_item);

    let language_menu = Submenu::with_items(
        app,
        lang_name,
        true,
        &[&*lang_zh_sc, &*lang_zh_tc, &*lang_en],
    )?;

    // === 窗口菜单 ===
    let win_minimize = MenuItem::with_id(
        app,
        "win-minimize".to_string(),
        minimize_text,
        true,
        None::<String>,
    )?;
    let win_maximize = MenuItem::with_id(
        app,
        "win-maximize".to_string(),
        maximize_text,
        true,
        None::<String>,
    )?;
    let win_close = MenuItem::with_id(
        app,
        "win-close".to_string(),
        close_text,
        true,
        None::<String>,
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

    let lang_zh_sc_config = LangItem { item: lang_zh_sc };
    let lang_zh_tc_config = LangItem { item: lang_zh_tc };
    let lang_en_config = LangItem { item: lang_en };

    Ok((menu, lang_zh_sc_config, lang_zh_tc_config, lang_en_config))
}
