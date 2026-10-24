/// Tauri 原生菜单栏定义
///
/// 设计要点：
/// 1. **表驱动**：所有菜单项写在 [`MENU_DEFS`] 一张表里（ID / 语言包 key / 快捷键），
///    建菜单、切换语言、导出「快捷键一览」都读同一份数据，新增菜单项只需改表 + 语言包。
/// 2. **就地更新**：切换语言只调用 `set_text`，不重建菜单
///    （重建时会丢一帧菜单栏，webview 被拉伸表现为界面抖动）。
/// 3. **状态联动**：前端把界面状态推送过来，[`MenuHandle::apply_state`]
///    据此启用/禁用与勾选菜单项，避免点了没反应或点到不该点的项。
///
/// 所有菜单文本均从 @src/i18n/locales/ 下的 JSON 文件读取，
/// Rust 代码中不硬编码任何翻译文本。

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
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
/// 菜单的勾选与语言项生成全部数据驱动，不需要再改其他代码。
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
    let menu = val
        .get("menu")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));
    let language = val
        .get("language")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));
    (menu, language)
}

// ============================================================
// 菜单项定义表
// ============================================================

/// 顶层子菜单的 key（同时也是语言包 menu 区块下的 key）
const SUBMENU_KEYS: &[&str] = &["app", "file", "run", "view", "language", "window", "help"];

/// 单条菜单项定义
pub struct ItemDef {
    /// 菜单项 ID，事件分发按它匹配
    pub id: &'static str,
    /// 语言包 menu 区块下的 key
    pub key: &'static str,
    /// 系统快捷键（muda 语法，`CmdOrCtrl` 在 macOS 映射为 Cmd）
    pub accel: Option<&'static str>,
    /// true 表示可勾选项（目前用于模式单选）
    pub check: bool,
}

/// 普通菜单项
const fn item(
    id: &'static str,
    key: &'static str,
    accel: Option<&'static str>,
) -> ItemDef {
    ItemDef { id, key, accel, check: false }
}

/// 可勾选菜单项
const fn check(
    id: &'static str,
    key: &'static str,
    accel: Option<&'static str>,
) -> ItemDef {
    ItemDef { id, key, accel, check: true }
}

/// 全部菜单项定义：顺序即快捷键一览的展示顺序
///
/// 新增菜单项：这里加一行 + 5 份语言包的 menu 区块加一条 + 对应子菜单装配处引用 ID。
const MENU_DEFS: &[ItemDef] = &[
    // ---- 文件 ----
    item("file-export-run", "exportRun", Some("CmdOrCtrl+E")),
    item("file-export-sweep", "exportSweep", Some("CmdOrCtrl+Shift+E")),
    item("file-open-downloads", "openDownloads", None),
    // 非 macOS 没有应用菜单，退出放在文件菜单末尾
    #[cfg(not(target_os = "macos"))]
    item("quit", "quit", Some("CmdOrCtrl+Q")),
    // ---- 运行 ----
    item("run-send", "send", Some("CmdOrCtrl+Enter")),
    item("run-cancel-all", "cancelAll", Some("CmdOrCtrl+.")),
    item("run-clear", "clear", Some("CmdOrCtrl+Shift+K")),
    check("run-mode-chat", "modeChat", Some("CmdOrCtrl+1")),
    check("run-mode-sweep", "modeSweep", Some("CmdOrCtrl+2")),
    item("run-sweep-start", "sweepStart", Some("CmdOrCtrl+Shift+S")),
    item("run-sweep-stop", "sweepStop", Some("CmdOrCtrl+Shift+X")),
    // ---- 视图 ----
    item("view-zoom-in", "zoomIn", Some("CmdOrCtrl+=")),
    item("view-zoom-out", "zoomOut", Some("CmdOrCtrl+-")),
    item("view-zoom-reset", "zoomReset", Some("CmdOrCtrl+0")),
    item("view-fullscreen", "fullscreen", Some("F11")),
    // 重新加载会丢失当前测试结果，只在 debug 构建暴露
    #[cfg(debug_assertions)]
    item("view-reload", "reload", Some("CmdOrCtrl+R")),
    #[cfg(debug_assertions)]
    item("view-devtools", "devtools", Some("F12")),
    // ---- 窗口 ----
    item("win-minimize", "minimize", None),
    item("win-maximize", "maximize", None),
    item("win-close", "close", None),
    // ---- 帮助 ----
    item("help-shortcuts", "shortcuts", Some("CmdOrCtrl+/")),
    item("help-homepage", "homepage", None),
    item("help-docs", "docs", None),
    // macOS 的「关于」在应用菜单里
    #[cfg(not(target_os = "macos"))]
    item("about", "about", None),
    // ---- 应用（仅 macOS）----
    #[cfg(target_os = "macos")]
    item("about", "about", None),
    #[cfg(target_os = "macos")]
    item("hide", "hide", None),
    #[cfg(target_os = "macos")]
    item("hide-others", "hideOthers", None),
    #[cfg(target_os = "macos")]
    item("quit", "quit", Some("CmdOrCtrl+Q")),
];

/// 快捷键一览的数据行（前端弹窗渲染用）
///
/// 由后端统一提供，避免前端另写一份快捷键表导致前后不一致。
#[derive(Debug, Clone, Serialize)]
pub struct Shortcut {
    /// 菜单项 ID
    pub id: &'static str,
    /// 语言包 menu 区块下的 key，前端据此取显示名
    pub key: &'static str,
    /// 快捷键字符串（`CmdOrCtrl` 由前端按平台替换为 Cmd / Ctrl）
    pub accel: &'static str,
}

/// 导出所有带快捷键的菜单项
pub fn shortcuts() -> Vec<Shortcut> {
    MENU_DEFS
        .iter()
        .filter_map(|def| {
            def.accel.map(|accel| Shortcut { id: def.id, key: def.key, accel })
        })
        .collect()
}

// ============================================================
// 界面状态 → 菜单可用性
// ============================================================

/// 前端推送的界面状态，用于决定菜单项可用性与勾选
#[derive(Debug, Clone, Deserialize)]
pub struct MenuState {
    /// 当前模式："chat" | "sweep"
    pub mode: String,
    /// 是否有对话窗口正在流式接收
    pub sending: bool,
    /// 梯度扫描是否进行中
    pub sweep_running: bool,
    /// 是否有本轮汇总可导出
    pub has_run: bool,
    /// 是否有扫描结果可导出
    pub has_sweep: bool,
}

/// 由界面状态推导各菜单项的可用性（纯函数，便于单测）
fn enabled_map(state: &MenuState) -> [(&'static str, bool); 9] {
    let busy = state.sending || state.sweep_running;
    // 两个模式的界面元素互斥渲染：对话模式看不到扫描面板，反之亦然，
    // 因此动作类菜单项也要按模式禁用，避免“跑了但看不见结果”
    let chat = state.mode != "sweep";
    [
        // 发送只属于对话模式：扫描面板里发出去也看不到窗口输出
        ("run-send", !busy && chat),
        ("run-cancel-all", busy),
        // 扫描控制只属于扫描模式：对话模式下扫描会静默跑完，结果无处展示
        ("run-sweep-start", !busy && !chat),
        ("run-sweep-stop", state.sweep_running),
        // 扫描中不能清空：清完扫描还会继续追加快照，容易让人误判
        ("run-clear", !state.sweep_running),
        // 扫描中不能切模式；对话请求在途时也不能切到扫描
        ("run-mode-chat", !state.sweep_running),
        ("run-mode-sweep", !state.sending),
        // 没有结果时导出无从下手
        ("file-export-run", state.has_run),
        ("file-export-sweep", state.has_sweep),
    ]
}

// ============================================================
// 语言包文本
// ============================================================

/// 单个语言的菜单文本集合
struct MenuTexts {
    /// 语言包 menu 区块（key → 文本）
    map: HashMap<String, String>,
    /// 语言子菜单中各选项的显示名（语言代码, 显示名）
    lang_names: Vec<(&'static str, String)>,
}

impl MenuTexts {
    /// 取菜单文本；key 缺失时回退 key 本身，单测据此断言语言包完整性
    fn text(&self, key: &str) -> String {
        self.map.get(key).cloned().unwrap_or_else(|| key.to_string())
    }

    /// 取某种语言自身的显示名，缺失时回退语言代码
    fn lang_name(&self, code: &str) -> String {
        self.lang_names
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| code.to_string())
    }
}

/// 按语言代码解析菜单文本（全部来自 locales JSON，保证前后端翻译一致）
fn menu_texts(lang: &str) -> MenuTexts {
    let (menu, language) = parse_locale(locale_json(lang));

    let map = match menu.as_object() {
        Some(obj) => obj
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect(),
        None => HashMap::new(),
    };

    // 语言名统一使用该语言自己的写法（如「日本語」）
    let lang_names = LANGS
        .iter()
        .map(|code| (*code, language.get(*code).and_then(|v| v.as_str()).unwrap_or(code).to_string()))
        .collect();

    MenuTexts { map, lang_names }
}

// ============================================================
// 菜单句柄
// ============================================================

/// 可随语言与界面状态变化的菜单句柄
///
/// 切换语言时只就地更新文本，**不重建菜单**。
///
/// 为什么不重建：`AppHandle::set_menu()` 内部会先 `remove_menu()`，再通过
/// `run_on_main_thread` 把新菜单异步挂到窗口上。中间那一帧菜单栏不存在，
/// 窗口内容会拖满整个高度（webview 被拉伸），下一帧才回缩，
/// 表现为切换语言时界面“抱一下”，而且中英文菜单栏行高不同时更明显。
///
/// 菜单项本身支持就地改文本（muda 的 GTK 实现直接调 `set_label`），
/// 因此完全不需要重建菜单。
pub struct MenuHandle<R: Runtime> {
    /// 顶层子菜单：key 即语言包 menu 区块下的 key（file/run/view/...）
    submenus: HashMap<&'static str, Arc<Submenu<R>>>,
    /// 普通菜单项：ID → 句柄
    items: HashMap<&'static str, Arc<MenuItem<R>>>,
    /// 可勾选菜单项（模式单选）：ID → 句柄
    checks: HashMap<&'static str, Arc<CheckMenuItem<R>>>,
    /// 语言选项：(语言代码, 复选项)，顺序与 [`LANGS`] 一致
    languages: Vec<(&'static str, Arc<CheckMenuItem<R>>)>,
}

impl<R: Runtime> MenuHandle<R> {
    /// 在不重建菜单的前提下应用指定语言的文本与勾选状态
    ///
    /// 单个菜单项更新失败（例如窗口已销毁）不影响其余项，因统一忽略返回值。
    pub fn apply_language(&self, lang: &str) {
        let texts = menu_texts(lang);

        // 子菜单标题：map 的 key 同时就是语言包的 key
        for (key, submenu) in &self.submenus {
            let _ = submenu.set_text(texts.text(key));
        }

        // 普通项与可勾选项：文本统一走 MENU_DEFS
        for def in MENU_DEFS {
            if let Some(item) = self.items.get(def.id) {
                let _ = item.set_text(texts.text(def.key));
            }
            if let Some(item) = self.checks.get(def.id) {
                let _ = item.set_text(texts.text(def.key));
            }
        }

        // 语言项：显示名 + 勾选状态
        for (code, item) in &self.languages {
            let _ = item.set_text(texts.lang_name(code));
            let _ = item.set_checked(*code == lang);
        }
    }

    /// 按前端界面状态启用/禁用、勾选菜单项
    pub fn apply_state(&self, state: &MenuState) {
        for (id, enabled) in enabled_map(state) {
            if let Some(item) = self.items.get(id) {
                let _ = item.set_enabled(enabled);
            }
            if let Some(item) = self.checks.get(id) {
                let _ = item.set_enabled(enabled);
            }
        }

        // 模式单选：当前模式打勾
        if let Some(item) = self.checks.get("run-mode-chat") {
            let _ = item.set_checked(state.mode != "sweep");
        }
        if let Some(item) = self.checks.get("run-mode-sweep") {
            let _ = item.set_checked(state.mode == "sweep");
        }
    }
}

// ============================================================
// 菜单构建
// ============================================================

/// 按 ID 取普通项引用（ID 来自 [`MENU_DEFS`]，缺失说明表与装配代码不一致）
fn item_ref<'a, R: Runtime>(
    items: &'a HashMap<&'static str, Arc<MenuItem<R>>>,
    id: &str,
) -> &'a dyn IsMenuItem<R> {
    items.get(id).expect("[menu] 菜单项未创建").as_ref()
}

/// 按 ID 取可勾选项引用
fn check_ref<'a, R: Runtime>(
    checks: &'a HashMap<&'static str, Arc<CheckMenuItem<R>>>,
    id: &str,
) -> &'a dyn IsMenuItem<R> {
    checks.get(id).expect("[menu] 菜单项未创建").as_ref()
}

/// 创建应用的原生菜单栏
///
/// 菜单结构（详见 README 或 `docs/ADDING_LANGUAGE.md`）：
/// - **应用**（仅 macOS）：关于、隐藏、隐藏其他、退出
/// - **文件**：导出本轮 CSV、导出扫描 CSV、打开下载目录、（非 macOS）退出
/// - **运行**：发送、全部取消、清空、模式单选、扫描开始/停止
/// - **视图**：缩放、全屏、（debug）重新加载、开发者工具
/// - **语言**：由 [`LANGS`] 动态生成（带勾选提示）
/// - **窗口**：最小化、最大化/还原、关闭
/// - **帮助**：快捷键一览、项目主页、使用文档、（非 macOS）关于
///
/// # 参数
/// * `initial_lang` - 初始化语言代码，用于确定菜单文本与初始勾选状态
///
/// # 返回值
/// * `(Menu, MenuHandle)` - 主菜单，以及可更新文本与状态的菜单项句柄
pub fn create_menu<R: Runtime>(
    app: &AppHandle<R>,
    initial_lang: &str,
) -> tauri::Result<(Menu<R>, MenuHandle<R>)> {
    let texts = menu_texts(initial_lang);

    // 启动时的初始状态：对话模式、无请求、无结果
    // 先按它建菜单，避免启动瞬间「全部取消 / 导出 / 停止扫描」全部可点
    let initial_state = MenuState {
        mode: "chat".to_string(),
        sending: false,
        sweep_running: false,
        has_run: false,
        has_sweep: false,
    };
    let rules = enabled_map(&initial_state);
    // 未列入状态表的菜单项（窗口/视图/帮助等）恒可用
    let is_enabled = |id: &str| {
        rules
            .iter()
            .find(|(item_id, _)| *item_id == id)
            .map(|(_, enabled)| *enabled)
            .unwrap_or(true)
    };

    // === 1) 先建好全部菜单项并放进 map，后续装配子菜单时按 ID 引用 ===
    let mut items: HashMap<&'static str, Arc<MenuItem<R>>> = HashMap::new();
    let mut checks: HashMap<&'static str, Arc<CheckMenuItem<R>>> = HashMap::new();
    for def in MENU_DEFS {
        if def.check {
            // 模式单选默认勾选对话模式（与 initial_state 一致）
            let checked = def.id == "run-mode-chat";
            let item = CheckMenuItem::with_id(
                app,
                def.id,
                texts.text(def.key),
                is_enabled(def.id),
                checked,
                def.accel,
            )?;
            checks.insert(def.id, Arc::new(item));
        } else {
            let item = MenuItem::with_id(
                app,
                def.id,
                texts.text(def.key),
                is_enabled(def.id),
                def.accel,
            )?;
            items.insert(def.id, Arc::new(item));
        }
    }

    // === 2) 语言子菜单：按 LANGS 动态生成，默认勾选当前语言 ===
    // 菜单项 ID 固定为 `lang-<语言代码>`，lib.rs 的事件分发按这个前缀匹配
    let mut languages: Vec<(&'static str, Arc<CheckMenuItem<R>>)> = Vec::with_capacity(LANGS.len());
    for code in LANGS {
        let item = CheckMenuItem::with_id(
            app,
            format!("lang-{code}"),
            texts.lang_name(code),
            true,
            code == initial_lang,
            None::<String>,
        )?;
        languages.push((code, Arc::new(item)));
    }

    // === 3) 装配各子菜单（分隔线需要具名持有，引用才能活到 with_items 调用结束）===
    let mut submenus: HashMap<&'static str, Arc<Submenu<R>>> = HashMap::new();

    // 应用菜单（仅 macOS）：关于 / 隐藏 / 隐藏其他 / 退出
    #[cfg(target_os = "macos")]
    {
        let sep1 = PredefinedMenuItem::separator(app)?;
        let sep2 = PredefinedMenuItem::separator(app)?;
        let refs: Vec<&dyn IsMenuItem<R>> = vec![
            item_ref(&items, "about"),
            &sep1,
            item_ref(&items, "hide"),
            item_ref(&items, "hide-others"),
            &sep2,
            item_ref(&items, "quit"),
        ];
        submenus.insert(
            "app",
            Arc::new(Submenu::with_items(app, texts.text("app"), true, &refs)?),
        );
    }

    // 文件：导出 → 打开下载目录 →（非 macOS）退出
    let file_sep1 = PredefinedMenuItem::separator(app)?;
    let mut file_refs: Vec<&dyn IsMenuItem<R>> = vec![
        item_ref(&items, "file-export-run"),
        item_ref(&items, "file-export-sweep"),
        &file_sep1,
        item_ref(&items, "file-open-downloads"),
    ];
    #[cfg(not(target_os = "macos"))]
    let file_sep2 = PredefinedMenuItem::separator(app)?;
    #[cfg(not(target_os = "macos"))]
    {
        file_refs.push(&file_sep2);
        file_refs.push(item_ref(&items, "quit"));
    }
    submenus.insert(
        "file",
        Arc::new(Submenu::with_items(app, texts.text("file"), true, &file_refs)?),
    );

    // 运行：动作 → 模式单选 → 扫描控制
    let run_sep1 = PredefinedMenuItem::separator(app)?;
    let run_sep2 = PredefinedMenuItem::separator(app)?;
    let run_refs: Vec<&dyn IsMenuItem<R>> = vec![
        item_ref(&items, "run-send"),
        item_ref(&items, "run-cancel-all"),
        item_ref(&items, "run-clear"),
        &run_sep1,
        check_ref(&checks, "run-mode-chat"),
        check_ref(&checks, "run-mode-sweep"),
        &run_sep2,
        item_ref(&items, "run-sweep-start"),
        item_ref(&items, "run-sweep-stop"),
    ];
    submenus.insert(
        "run",
        Arc::new(Submenu::with_items(app, texts.text("run"), true, &run_refs)?),
    );

    // 视图：缩放 → 全屏 →（debug）重新加载 / 开发者工具
    let view_sep1 = PredefinedMenuItem::separator(app)?;
    let mut view_refs: Vec<&dyn IsMenuItem<R>> = vec![
        item_ref(&items, "view-zoom-in"),
        item_ref(&items, "view-zoom-out"),
        item_ref(&items, "view-zoom-reset"),
        &view_sep1,
        item_ref(&items, "view-fullscreen"),
    ];
    #[cfg(debug_assertions)]
    let view_sep2 = PredefinedMenuItem::separator(app)?;
    #[cfg(debug_assertions)]
    {
        view_refs.push(&view_sep2);
        view_refs.push(item_ref(&items, "view-reload"));
        view_refs.push(item_ref(&items, "view-devtools"));
    }
    submenus.insert(
        "view",
        Arc::new(Submenu::with_items(app, texts.text("view"), true, &view_refs)?),
    );

    // 语言
    let language_refs: Vec<&dyn IsMenuItem<R>> = languages
        .iter()
        .map(|(_, item)| item.as_ref() as &dyn IsMenuItem<R>)
        .collect();
    submenus.insert(
        "language",
        Arc::new(Submenu::with_items(
            app,
            texts.text("language"),
            true,
            &language_refs,
        )?),
    );

    // 窗口
    let window_sep = PredefinedMenuItem::separator(app)?;
    let window_refs: Vec<&dyn IsMenuItem<R>> = vec![
        item_ref(&items, "win-minimize"),
        item_ref(&items, "win-maximize"),
        &window_sep,
        item_ref(&items, "win-close"),
    ];
    submenus.insert(
        "window",
        Arc::new(Submenu::with_items(app, texts.text("window"), true, &window_refs)?),
    );

    // 帮助：快捷键 / 链接 →（非 macOS）关于
    let mut help_refs: Vec<&dyn IsMenuItem<R>> = vec![
        item_ref(&items, "help-shortcuts"),
        item_ref(&items, "help-homepage"),
        item_ref(&items, "help-docs"),
    ];
    #[cfg(not(target_os = "macos"))]
    let help_sep = PredefinedMenuItem::separator(app)?;
    #[cfg(not(target_os = "macos"))]
    {
        help_refs.push(&help_sep);
        help_refs.push(item_ref(&items, "about"));
    }
    submenus.insert(
        "help",
        Arc::new(Submenu::with_items(app, texts.text("help"), true, &help_refs)?),
    );

    // === 4) 顶层菜单栏：顺序由 [`SUBMENU_KEYS`] 决定 ===
    // macOS 的「应用」菜单必须排在最前（非 macOS 下该子菜单不存在，会被自动跳过）
    let top_refs: Vec<&dyn IsMenuItem<R>> = SUBMENU_KEYS
        .iter()
        .filter_map(|key| submenus.get(key).map(|sub| sub.as_ref() as &dyn IsMenuItem<R>))
        .collect();
    let menu = Menu::with_items(app, &top_refs)?;

    let handle = MenuHandle { submenus, items, checks, languages };
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

    /// 所有菜单项与子菜单的语言 key 必须在每份语言包中都有翻译
    #[test]
    fn test_all_menu_keys_translated() {
        for ui_lang in LANGS {
            let texts = menu_texts(ui_lang);
            for key in SUBMENU_KEYS {
                assert_ne!(
                    texts.text(key),
                    *key,
                    "语言包 {ui_lang} 缺少 menu.{key}"
                );
            }
            for def in MENU_DEFS {
                assert_ne!(
                    texts.text(def.key),
                    def.key,
                    "语言包 {ui_lang} 缺少 menu.{}",
                    def.key
                );
            }
        }
    }

    /// 菜单 ID 必须唯一，否则事件分发会命中错误分支
    #[test]
    fn test_menu_ids_unique() {
        let mut seen = std::collections::HashSet::new();
        for def in MENU_DEFS {
            assert!(seen.insert(def.id), "菜单 ID 重复: {}", def.id);
        }
        // 语言项 ID 约定为 `lang-<代码>`，同样不能与普通项冲突
        for code in LANGS {
            let id = format!("lang-{code}");
            assert!(!seen.contains(id.as_str()), "菜单 ID 与语言项冲突: {id}");
        }
    }

    /// 快捷键不得重复，否则后注册的会互相抢占
    #[test]
    fn test_accelerators_unique() {
        let mut seen = std::collections::HashSet::new();
        for def in MENU_DEFS {
            if let Some(accel) = def.accel {
                assert!(seen.insert(accel), "快捷键重复: {accel}");
            }
        }
    }

    /// 快捷键必须形如「修饰键+主键」，避免运行期解析失败导致菜单创建报错
    #[test]
    fn test_accelerator_format() {
        for def in MENU_DEFS {
            if let Some(accel) = def.accel {
                let main_key = accel.rsplit('+').next().unwrap_or("");
                assert!(!main_key.is_empty(), "快捷键缺少主键: {accel}");
            }
        }
    }

    /// 未知语言应回退英文，而不是产生空文本或 panic
    #[test]
    fn test_unknown_language_falls_back_to_english() {
        let unknown = menu_texts("fr");
        let english = menu_texts("en");
        assert_eq!(unknown.text("file"), english.text("file"));
        assert_eq!(unknown.text("run"), english.text("run"));
        // 即使界面语言未知，语言菜单仍需列出全部受支持语言
        assert_eq!(unknown.lang_names.len(), LANGS.len());
    }

    /// 状态联动规则：发送中只允许取消，扫描中不允许切模式与重复开始
    #[test]
    fn test_enabled_map() {
        let idle = MenuState {
            mode: "chat".to_string(),
            sending: false,
            sweep_running: false,
            has_run: false,
            has_sweep: false,
        };
        let value = |state: &MenuState, id: &str| {
            enabled_map(state)
                .into_iter()
                .find(|(item_id, _)| *item_id == id)
                .map(|(_, enabled)| enabled)
                .unwrap_or_else(|| panic!("状态表缺少 {id}"))
        };

        // 空闲：可发送、不可取消、无结果时不能导出
        assert!(value(&idle, "run-send"));
        assert!(!value(&idle, "run-cancel-all"));
        assert!(!value(&idle, "file-export-run"));
        assert!(!value(&idle, "file-export-sweep"));
        assert!(!value(&idle, "run-sweep-stop"));
        // 对话模式下不能开始扫描（否则静默跑完也看不到结果）
        assert!(!value(&idle, "run-sweep-start"));

        // 模式隔离：扫描模式下不能发送，但可以开始扫描
        let sweep_idle = MenuState { mode: "sweep".to_string(), ..idle.clone() };
        assert!(!value(&sweep_idle, "run-send"));
        assert!(value(&sweep_idle, "run-sweep-start"));

        // 对话发送中：只能取消，且不能切到扫描模式
        let sending = MenuState { sending: true, ..idle.clone() };
        assert!(!value(&sending, "run-send"));
        assert!(value(&sending, "run-cancel-all"));
        assert!(!value(&sending, "run-mode-sweep"));
        assert!(value(&sending, "run-mode-chat"));

        // 扫描中：只能停止，不能切回对话模式，也不能清空结果
        let sweeping = MenuState { sweep_running: true, ..idle.clone() };
        assert!(!value(&sweeping, "run-sweep-start"));
        assert!(value(&sweeping, "run-sweep-stop"));
        assert!(!value(&sweeping, "run-mode-chat"));
        assert!(!value(&sweeping, "run-clear"));

        // 有结果后导出可用
        let done = MenuState { has_run: true, has_sweep: true, ..idle };
        assert!(value(&done, "file-export-run"));
        assert!(value(&done, "file-export-sweep"));
    }
}
