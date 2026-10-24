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
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    AppHandle, Runtime,
};

// ============================================================
// 从 locales JSON 文件编译期嵌入翻译数据
// 路径相对于 src-tauri/src/menu.rs，使用 ../../ 回到项目根目录
// ============================================================

/// 支持的语言代码，顺序即菜单中的展示顺序
///
/// 新增语言时只需三步：① 在 locales 下新增 JSON 语言包；
/// ② 在 [`locale_json`] 里加一条映射；③ 在前端 `src/i18n/index.ts` 注册。
/// 菜单的勾选与重建逻辑全部数据驱动，不需要再改其他代码。
pub const LANGS: [&str; 5] = ["zh-SC", "zh-TC", "en", "ja", "ko"];

/// 语言代码 → 编译期嵌入的语言包 JSON
fn locale_json(lang: &str) -> &'static str {
    match lang {
        "zh-SC" => include_str!("../../src/i18n/locales/zh-SC.json"),
        "zh-TC" => include_str!("../../src/i18n/locales/zh-TC.json"),
        "en" => include_str!("../../src/i18n/locales/en.json"),
        "ja" => include_str!("../../src/i18n/locales/ja.json"),
        "ko" => include_str!("../../src/i18n/locales/ko.json"),
        // 未知语言回退英文，避免菜单出现空文本
        _ => include_str!("../../src/i18n/locales/en.json"),
    }
}

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

/// 单个语言的菜单文本集合
struct MenuTexts {
    app: String,
    about: String,
    quit: String,
    /// 仅 macOS 使用
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    hide: String,
    /// 仅 macOS 使用
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    hide_others: String,
    minimize: String,
    maximize: String,
    close: String,
    language: String,
    window: String,
    /// 语言子菜单中各选项的显示名（从当前语言包的 language 区块读取）
    lang_names: Vec<(&'static str, String)>,
}

/// 按语言代码解析菜单文本
///
/// 所有文本均来自 locales JSON 文件，确保前后端翻译一致。
fn menu_texts(lang: &str) -> MenuTexts {
    let (menu, language) = parse_locale(locale_json(lang));

    // 语言名统一使用该语言自己的写法（如「日本語」），缺失时回退语言代码
    let lang_names = LANGS
        .iter()
        .map(|code| {
            let name = language
                .get(*code)
                .and_then(|v| v.as_str())
                .unwrap_or(code)
                .to_string();
            (*code, name)
        })
        .collect();

    MenuTexts {
        app: str_val(&menu, "app"),
        about: str_val(&menu, "about"),
        quit: str_val(&menu, "quit"),
        hide: str_val(&menu, "hide"),
        hide_others: str_val(&menu, "hideOthers"),
        minimize: str_val(&menu, "minimize"),
        maximize: str_val(&menu, "maximize"),
        close: str_val(&menu, "close"),
        language: str_val(&menu, "language"),
        window: str_val(&menu, "window"),
        lang_names,
    }
}

/// 可随语言变化的菜单项集合
///
/// 切换语言时只就地更新文本与勾选状态，**不重建菜单**。
///
/// 为什么不重建：`AppHandle::set_menu()` 内部会先 `remove_menu()`，再通过
/// `run_on_main_thread` 把新菜单异步挂到窗口上。中间那一帧菜单栏不存在，
/// 窗口内容会拖满整个高度（webview 被拉伸），下一帧才回缩，
/// 表现为切换语言时界面“抱一下”，而且中英文菜单栏行高不同时更明显。
///
/// 菜单项本身支持就地改文本（muda 的 GTK 实现直接调 `set_label`），
/// 因此完全不需要重建菜单。
pub struct MenuHandle<R: Runtime> {
    /// 顶层子菜单：应用 / 语言 / 窗口
    app_menu: Arc<Submenu<R>>,
    language_menu: Arc<Submenu<R>>,
    window_menu: Arc<Submenu<R>>,
    /// 应用菜单内的普通项
    about: Arc<MenuItem<R>>,
    quit: Arc<MenuItem<R>>,
    /// 仅 macOS 使用
    #[cfg(target_os = "macos")]
    hide: Arc<MenuItem<R>>,
    /// 仅 macOS 使用
    #[cfg(target_os = "macos")]
    hide_others: Arc<MenuItem<R>>,
    /// 窗口菜单内的普通项
    minimize: Arc<MenuItem<R>>,
    maximize: Arc<MenuItem<R>>,
    close: Arc<MenuItem<R>>,
    /// 语言选项：(语言代码, 复选项)，顺序与 [`LANGS`] 一致
    languages: Vec<(&'static str, Arc<CheckMenuItem<R>>)>,
}

impl<R: Runtime> MenuHandle<R> {
    /// 在不重建菜单的前提下应用指定语言的文本与勾选状态
    ///
    /// 单个菜单项更新失败（例如窗口已销毁）不影响其余项，因统一忽略返回值。
    pub fn apply_language(&self, lang: &str) {
        let texts = menu_texts(lang);

        let _ = self.app_menu.set_text(texts.app);
        let _ = self.about.set_text(texts.about);
        let _ = self.quit.set_text(texts.quit);
        #[cfg(target_os = "macos")]
        {
            let _ = self.hide.set_text(texts.hide);
            let _ = self.hide_others.set_text(texts.hide_others);
        }
        let _ = self.language_menu.set_text(texts.language);
        let _ = self.window_menu.set_text(texts.window);
        let _ = self.minimize.set_text(texts.minimize);
        let _ = self.maximize.set_text(texts.maximize);
        let _ = self.close.set_text(texts.close);

        // 勾选状态：仅当前语言被选中
        for (code, item) in &self.languages {
            let _ = item.set_checked(*code == lang);
        }
    }
}

/// 创建应用的原生菜单栏
///
/// 当前包含以下菜单：
/// - **应用**：关于、分隔线、退出
/// - **语言**：由 [`LANGS`] 动态生成（带勾选提示）
/// - **窗口**：最小化、最大化/还原、关闭
///
/// 返回的 [`MenuHandle`] 供后续切换语言时就地更新文本，无需重建菜单。
///
/// # 参数
/// * `initial_lang` - 初始化语言代码，用于确定菜单文本与初始勾选状态
///
/// # 返回值
/// * `(Menu, MenuHandle)` - 主菜单，以及可更新文本的菜单项句柄
pub fn create_menu<R: Runtime>(
    app: &AppHandle<R>,
    initial_lang: &str,
) -> tauri::Result<(Menu<R>, MenuHandle<R>)> {
    let texts = menu_texts(initial_lang);

    // === 应用菜单（用普通 MenuItem 而非预定义项，才能自行控制文本与语言） ===
    let about_item = Arc::new(MenuItem::with_id(
        app,
        "about".to_string(),
        texts.about,
        true,
        None::<String>,
    )?);
    let quit_item = Arc::new(MenuItem::with_id(
        app,
        "quit".to_string(),
        texts.quit,
        true,
        None::<String>,
    )?);
    #[cfg(target_os = "macos")]
    let hide_item = Arc::new(MenuItem::with_id(
        app,
        "hide".to_string(),
        texts.hide,
        true,
        None::<String>,
    )?);
    #[cfg(target_os = "macos")]
    let hide_others_item = Arc::new(MenuItem::with_id(
        app,
        "hide-others".to_string(),
        texts.hide_others,
        true,
        None::<String>,
    )?);

    let app_menu = Arc::new(Submenu::with_items(
        app,
        texts.app,
        true,
        &[
            &*about_item,
            &PredefinedMenuItem::separator(app)?,
            #[cfg(target_os = "macos")]
            &*hide_item,
            #[cfg(target_os = "macos")]
            &*hide_others_item,
            #[cfg(target_os = "macos")]
            &PredefinedMenuItem::separator(app)?,
            &*quit_item,
        ],
    )?);

    // === 语言菜单：按 LANGS 动态生成，默认勾选当前语言 ===
    // 菜单项 ID 固定为 `lang-<语言代码>`，lib.rs 的事件分发也依赖这个约定
    let mut languages: Vec<(&'static str, Arc<CheckMenuItem<R>>)> = Vec::with_capacity(LANGS.len());
    for (code, name) in texts.lang_names {
        let item = CheckMenuItem::with_id(
            app,
            format!("lang-{code}"),
            name,
            true,
            code == initial_lang,
            None::<String>,
        )?;
        languages.push((code, Arc::new(item)));
    }

    let language_menu = {
        // with_items 需要 `&[&dyn IsMenuItem]`，先收集引用再构建，借用只在这个块内有效
        let refs: Vec<&dyn IsMenuItem<R>> = languages
            .iter()
            .map(|(_, item)| item.as_ref() as &dyn IsMenuItem<R>)
            .collect();
        Arc::new(Submenu::with_items(app, texts.language, true, &refs)?)
    };

    // === 窗口菜单 ===
    let win_minimize = Arc::new(MenuItem::with_id(
        app,
        "win-minimize".to_string(),
        texts.minimize,
        true,
        None::<String>,
    )?);
    let win_maximize = Arc::new(MenuItem::with_id(
        app,
        "win-maximize".to_string(),
        texts.maximize,
        true,
        None::<String>,
    )?);
    let win_close = Arc::new(MenuItem::with_id(
        app,
        "win-close".to_string(),
        texts.close,
        true,
        None::<String>,
    )?);

    let window_menu = Arc::new(Submenu::with_items(
        app,
        texts.window,
        true,
        &[
            &*win_minimize,
            &*win_maximize,
            &PredefinedMenuItem::separator(app)?,
            &*win_close,
        ],
    )?);

    let menu = Menu::with_items(
        app,
        &[&*app_menu, &*language_menu, &*window_menu],
    )?;

    let handle = MenuHandle {
        app_menu,
        language_menu,
        window_menu,
        about: about_item,
        quit: quit_item,
        #[cfg(target_os = "macos")]
        hide: hide_item,
        #[cfg(target_os = "macos")]
        hide_others: hide_others_item,
        minimize: win_minimize,
        maximize: win_maximize,
        close: win_close,
        languages,
    };

    Ok((menu, handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个语言包都必须为所有受支持语言提供显示名，
    /// 否则对应菜单项会退化成语言代码（如显示成 "ja"）
    #[test]
    fn test_all_locales_provide_language_names() {
        for ui_lang in LANGS {
            let texts = menu_texts(ui_lang);
            assert_eq!(texts.lang_names.len(), LANGS.len());
            for (code, name) in &texts.lang_names {
                assert_ne!(
                    name.as_str(),
                    *code,
                    "语言包 {ui_lang} 缺少语言 {code} 的显示名"
                );
            }
        }
    }

    /// 菜单文本必须来自语言包；若 key 不存在，str_val 会回退成 key 本身
    #[test]
    fn test_menu_texts_are_translated() {
        for ui_lang in LANGS {
            let texts = menu_texts(ui_lang);
            assert_ne!(texts.app, "app", "语言包 {ui_lang} 缺少 menu.app");
            assert_ne!(
                texts.language, "language",
                "语言包 {ui_lang} 缺少 menu.language"
            );
            assert_ne!(texts.window, "window", "语言包 {ui_lang} 缺少 menu.window");
            assert_ne!(texts.quit, "quit", "语言包 {ui_lang} 缺少 menu.quit");
        }
    }

    /// 未知语言应回退英文，而不是产生空文本或 panic
    #[test]
    fn test_unknown_language_falls_back_to_english() {
        let unknown = menu_texts("fr");
        let english = menu_texts("en");
        assert_eq!(unknown.app, english.app);
        assert_eq!(unknown.window, english.window);
        // 即使界面语言未知，语言菜单仍需列出全部受支持语言
        assert_eq!(unknown.lang_names.len(), LANGS.len());
    }
}
