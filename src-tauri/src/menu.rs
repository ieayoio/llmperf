/// Tauri 原生菜单栏定义
///
/// 使用 Tauri 2.11 的原生菜单系统，提供带勾选提示的语言切换功能。
/// 点击菜单项时会触发 menu_event，前端通过监听事件来切换语言。
/// 菜单文本会随语言切换通过重建菜单来动态更新。
///
/// 所有菜单文本均从 @src/i18n/locales/ 下的 JSON 文件读取，
/// 无需在 Rust 代码中硬编码任何翻译文本。

use std::sync::Arc;
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    AppHandle, Runtime,
};

// ============================================================
// 从 locales JSON 文件编译期嵌入翻译数据
// 路径相对于 src-tauri/src/menu.rs，使用 ../../ 回到项目根目录
// ============================================================

/// 简体中文语言包（编译期嵌入）
const LOCALE_ZH_SC: &str = include_str!("../../src/i18n/locales/zh-SC.json");
/// 繁体中文语言包（编译期嵌入）
const LOCALE_ZH_TC: &str = include_str!("../../src/i18n/locales/zh-TC.json");
/// 英文语言包（编译期嵌入）
const LOCALE_EN: &str = include_str!("../../src/i18n/locales/en.json");

/// 从嵌入的 JSON 字符串中解析出 menu 和 language 区块
/// 返回 (menu_json, language_json)
fn parse_locale(json_str: &str) -> (serde_json::Value, serde_json::Value) {
    let val: serde_json::Value =
        serde_json::from_str(json_str).expect("[menu] 无法解析语言包 JSON");
    let menu = val.get("menu").cloned().unwrap_or(serde_json::Value::Object(Default::default()));
    let language = val
        .get("language")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));
    (menu, language)
}

/// 从 JSON Value 中安全获取字符串字段
fn str_val(val: &serde_json::Value, key: &str) -> String {
    val.get(key).and_then(|v| v.as_str()).unwrap_or(key).to_string()
}

/// 根据语言代码获取对应的菜单文本和语言选项名称
///
/// 所有文本均来自 locales JSON 文件，确保前后端翻译一致。
///
/// 返回: (
///   app_name, about_text, quit_text, hide_text, hide_others_text,
///   minimize_text, maximize_text, close_text,
///   lang_zh_sc_name, lang_zh_tc_name, lang_name, window_name
/// )
fn menu_texts(lang: &str) -> (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
) {
    let (menu, language) = match lang {
        "zh-SC" => parse_locale(LOCALE_ZH_SC),
        "zh-TC" => parse_locale(LOCALE_ZH_TC),
        _ => parse_locale(LOCALE_EN),
    };

    (
        str_val(&menu, "app"),
        str_val(&menu, "about"),
        str_val(&menu, "quit"),
        str_val(&menu, "hide"),
        str_val(&menu, "hideOthers"),
        str_val(&menu, "minimize"),
        str_val(&menu, "maximize"),
        str_val(&menu, "close"),
        // 语言选项名称从 language 区块读取，始终使用当前语言的写法
        language.get("zh-SC").and_then(|v| v.as_str()).unwrap_or("简体中文").to_string(),
        language.get("zh-TC").and_then(|v| v.as_str()).unwrap_or("繁體中文").to_string(),
        str_val(&menu, "language"),
        str_val(&menu, "window"),
    )
}

/// 存储语言菜单项的容器，供 lib.rs 中的命令访问和更新
pub struct LangItems<R: Runtime> {
    /// 简体中文菜单项
    pub zh_sc: Arc<CheckMenuItem<R>>,
    /// 繁体中文菜单项
    pub zh_tc: Arc<CheckMenuItem<R>>,
    /// 英文菜单项
    pub en: Arc<CheckMenuItem<R>>,
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
/// * `(Menu, Arc<CheckMenuItem>, Arc<CheckMenuItem>, Arc<CheckMenuItem>)` - 主菜单、简体中文菜单项、繁体中文菜单项、英文菜单项
pub fn create_menu<R: Runtime>(
    app: &AppHandle<R>,
    initial_lang: &str,
) -> tauri::Result<(Menu<R>, Arc<CheckMenuItem<R>>, Arc<CheckMenuItem<R>>, Arc<CheckMenuItem<R>>)> {
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
    ) = menu_texts(initial_lang);
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

    Ok((menu, lang_zh_sc, lang_zh_tc, lang_en))
}
