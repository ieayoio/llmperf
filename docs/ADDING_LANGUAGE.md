# 添加新语言指南

本文档指导你如何为 LLM 并发测试工具添加新的语言版本。

## 工作原理

1. **Tauri 原生菜单**：在 Rust 后端 (`src-tauri/src/menu.rs`) 定义原生菜单栏
2. **语言包文件**：在 `src/i18n/locales/` 下存放各语言的 JSON 翻译文件
3. **前端监听**：前端通过 Tauri 事件系统监听语言切换，更新界面

## 快速开始（3 步搞定）

### 第 1 步：复制中文语言包

复制 `zh.json` 文件，用新语言代码命名：

```bash
cp src/i18n/locales/zh.json src/i18n/locales/ja.json
```

### 第 2 步：翻译内容

用你熟悉的语言替换所有 `value` 的值。

**重要规则：**
- ❌ 不要修改 `key`（如 `app.title`、`config.send` 等）
- ❌ 不要修改 `{0}`、`{1}` 等数字占位符
- ✅ 只修改引号内的文本内容

**示例：**
```json
// 原始（中文）
"app": {
  "title": "⚡ LLM 并发测试工具"
}

// 修改后（日语）
"app": {
  "title": "⚡ LLM 並行テストツール"
}
```

**关于占位符 `{0}`、`{1}`：**
如果翻译后的文本中需要嵌入动态数字，保留占位符即可。
例如 `"窗口 {0}"` → 日语 `"ウィンドウ {0}"`

### 第 3 步：注册语言

#### 3.1 修改 `src/i18n/index.ts`

打开 `src/i18n/index.ts` 文件，找到 `SUPPORTED_LANGUAGES` 部分：

```typescript
export const SUPPORTED_LANGUAGES: Record<string, { code: string; name: string }> = {
  zh: { code: "zh", name: "中文" },
  en: { code: "en", name: "English" },
  // ← 在这里添加你的新语言
  // ja: { code: "ja", name: "日本語" },
};
```

取消注释并修改为你的语言，例如添加日语：

```typescript
ja: { code: "ja", name: "日本語" },
```

然后在 `import` 部分添加语言包导入：

```typescript
import zh from "./locales/zh.json";
import en from "./locales/en.json";
import ja from "./locales/ja.json";  // ← 添加这行
```

最后，将语言包加入 `localeFiles` 字典：

```typescript
const localeFiles: Record<string, Record<string, unknown>> = {
  zh, en, ja,  // ← 添加 ja
};
```

#### 3.2 修改 `src-tauri/src/menu.rs`

在 `语言` 子菜单中添加新的菜单项：

```rust
// 在语言菜单部分添加新语言项
let lang_zh = MenuItem::with_id(app, "lang-zh".to_string(), "中文", true, None::<String>)?;
let lang_en = MenuItem::with_id(app, "lang-en".to_string(), "English", true, None::<String>)?;
let lang_ja = MenuItem::with_id(app, "lang-ja".to_string(), "日本語", true, None::<String>)?;  // ← 添加

let language_menu = Submenu::with_items(app, "语言", true, &[&lang_zh, &lang_en, &lang_ja])?;  // ← 添加 &lang_ja
```

#### 3.3 修改 `src-tauri/src/lib.rs`

在菜单事件处理器中添加新语言的判断：

```rust
app.on_menu_event(|app, event| {
    let id = event.id();
    if *id == "lang-zh" {
        let _ = app.emit_str("language-changed", "zh".to_string());
    } else if *id == "lang-en" {
        let _ = app.emit_str("language-changed", "en".to_string());
    } else if *id == "lang-ja" {
        let _ = app.emit_str("language-changed", "ja".to_string());  // ← 添加
    }
});
```

## 完整示例：添加日语 (ja)

### 1. 创建 `src/i18n/locales/ja.json`

```json
{
  "app": {
    "title": "⚡ LLM 並行テストツール"
  },
  "config": {
    "baseURL": "Base URL",
    "baseURLPlaceholder": "http://127.0.0.1:16777/v1",
    "apiKey": "API Key",
    "apiKeyPlaceholder": "sk-...",
    "model": "Model",
    "modelPlaceholder": "gpt-3.5-turbo",
    "concurrency": "並行数",
    "sendMessage": "メッセージ",
    "messagePlaceholder": "テストメッセージを入力 (Enter で送信)",
    "send": "🚀 送信",
    "clear": "🗑 クリア"
  },
  "status": {
    "idle": "⏸ 待機中",
    "sending": "⏳ 送信中... (本文 {0} 文字)",
    "sending_with_reasoning": "⏳ 送信中... (本文 {0} 文字, 思考 {1} 文字)",
    "done": "✅ 完了 ({0}ms)",
    "error": "❌ {0}ms",
    "content": "本文",
    "reasoning": "思考"
  },
  "chat": {
    "window": "ウィンドウ {0}",
    "empty": "送信待ち...",
    "user": "👤 あなた",
    "assistant": "🤖 ボット",
    "reasoning": "💭 思考中",
    "error": "エラー:",
    "reasoningSummary": "💭 思考プロセス"
  },
  "language": {
    "label": "言語",
    "zh": "中文",
    "en": "English",
    "menu": "言語"
  }
}
```

### 2. 修改 `src/i18n/index.ts`

```typescript
import ja from "./locales/ja.json";  // 添加导入

export const SUPPORTED_LANGUAGES = {
  zh: { code: "zh", name: "中文" },
  en: { code: "en", name: "English" },
  ja: { code: "ja", name: "日本語" },  // 添加注册
};

const localeFiles = {
  zh, en, ja,  // 添加 ja
};
```

### 3. 修改 `src-tauri/src/menu.rs`

```rust
let lang_ja = MenuItem::with_id(app, "lang-ja".to_string(), "日本語", true, None::<String>)?;
let language_menu = Submenu::with_items(app, "言語", true, &[&lang_zh, &lang_en, &lang_ja])?;
```

### 4. 修改 `src-tauri/src/lib.rs`

```rust
} else if *id == "lang-ja" {
    let _ = app.emit_str("language-changed", "ja".to_string());
}
```

## 常见问题

**Q: 翻译后界面没有变化？**
A: 检查是否在所有 3 个步骤中都添加了新语言。

**Q: 占位符 {0} 没有显示数字？**
A: 占位符必须保留，不要翻译或移除。

**Q: 可以添加不常见的语言吗？**
A: 可以！只要语言包格式正确，任何语言都可以添加。

## 文件位置总览

```
src/
├── i18n/
│   ├── index.ts              # 国际化核心逻辑（注册语言 + 翻译函数）
│   └── locales/
│       ├── zh.json           # 中文（参考模板）
│       ├── en.json           # 英文
│       └── ja.json           # 新增语言（按此格式添加）
├── components/               # （已删除前端语言菜单组件）
└── App.tsx                   # 监听 Tauri 事件，触发语言切换

src-tauri/src/
├── menu.rs                   # 原生菜单栏定义（添加菜单项）
└── lib.rs                    # 菜单事件处理（监听点击 → 通知前端）

docs/
└── ADDING_LANGUAGE.md        # 本文档
```
