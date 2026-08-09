/// Tauri 原生菜单栏定义
///
/// 使用 Tauri 2.11 的原生菜单系统，提供带勾选提示的语言切换功能。
/// 点击菜单项时会触发 menu_event，前端通过监听事件来切换语言。

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
/// - **窗口**：最小化
///
/// 勾选状态会随语言切换自动更新。
///
/// # 参数
/// * `initial_lang` - 初始化语言（"zh" 或 "en"），用于设置菜单项的初始勾选状态
pub fn create_menu<R: Runtime>(app: &AppHandle<R>, initial_lang: &str) -> tauri::Result<(Menu<R>, LangItem<R>, LangItem<R>)> {
    // === 应用菜单 ===
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

    // === 语言菜单：使用 CheckMenuItem 实现勾选提示 ===
    // 根据传入的 initial_lang 设置初始勾选状态
    let is_zh = initial_lang == "zh";
    let lang_zh_item = CheckMenuItem::with_id(
        app,
        "lang-zh".to_string(),
        "中文",
        true,
        is_zh, // 根据初始语言设置是否选中
        None::<String>,
    )?;
    let lang_zh = Arc::new(lang_zh_item);

    let lang_en_item = CheckMenuItem::with_id(
        app,
        "lang-en".to_string(),
        "English",
        true,
        !is_zh, // 根据初始语言设置是否选中
        None::<String>,
    )?;
    let lang_en = Arc::new(lang_en_item);

    let language_menu = Submenu::with_items(
        app,
        "语言",
        true,
        &[&*lang_zh, &*lang_en],
    )?;

    // === 窗口菜单：最小化、最大化/还原、关闭 ===
    // 使用自定义 MenuItem，在 on_menu_event 中手动调用窗口方法
    // （PredefinedMenuItem 的窗口操作用法在 Linux 上 Unsupported）
    let win_minimize = MenuItem::with_id(
        app, "win-minimize".to_string(), "最小化", true, None::<String>,
    )?;
    let win_maximize = MenuItem::with_id(
        app, "win-maximize".to_string(), "最大化", true, None::<String>,
    )?;
    let win_close = MenuItem::with_id(
        app, "win-close".to_string(), "关闭窗口", true, None::<String>,
    )?;

    let window_menu = Submenu::with_items(
        app,
        "窗口",
        true,
        &[
            &win_minimize,
            &win_maximize,
            &PredefinedMenuItem::separator(app)?,
            &win_close,
        ],
    )?;

    // 构建主菜单
    let menu = Menu::with_items(
        app,
        &[&app_menu, &language_menu, &window_menu],
    )?;

    // 返回菜单和语言项的引用，供事件处理器更新勾选状态
    let lang_zh_config = LangItem { item: lang_zh };
    let lang_en_config = LangItem { item: lang_en };

    Ok((menu, lang_zh_config, lang_en_config))
}
