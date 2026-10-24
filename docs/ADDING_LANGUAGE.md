# 添加新语言指南

本文档说明如何为 LLM 并发测试工具新增一种界面语言。当前已支持：简体中文（`zh-SC`）、繁体中文（`zh-TC`）、English（`en`）、日本語（`ja`）、한국어（`ko`）。

## 工作原理

翻译文本只有一份来源：`src/i18n/locales/*.json`。

| 位置 | 作用 |
|------|------|
| `src/i18n/locales/*.json` | 唯一的翻译来源，前端界面与 Rust 原生菜单都从这里取文本 |
| `src/i18n/index.ts` | 前端加载语言包、`t()` 翻译函数、持久化到 `localStorage` |
| `src-tauri/src/menu.rs` | 编译期用 `include_str!` 嵌入同一批 JSON，动态生成本地菜单 |

因为菜单项是**按 `LANGS` 数组动态生成**的，新增语言不需要逐个手写菜单项。

## 三步添加语言

### 第 1 步：新增语言包

复制 `zh-SC.json`（键最全）并按语言代码命名：

```bash
cp src/i18n/locales/zh-SC.json src/i18n/locales/xx.json
```

翻译规则：

- ❌ 不要改动 `key`（如 `config.send`、`summary.tpot`）
- ❌ 不要改动 `{0}`、`{1}` 等占位符，也不要删除
- ✅ 只替换引号内的文本

**必须在所有语言包里补齐新语言在 `language` 区块下的显示名**，否则该语言的菜单项会退化成语言代码：

```json
"language": {
  "label": "语言",
  "zh-SC": "简体中文",
  "zh-TC": "繁體中文",
  "en": "English",
  "ja": "日本語",
  "ko": "한국어",
  "xx": "Your Language"
}
```

语言名统一用该语言自己的写法（日本語、한국어），不随界面语言变化。

### 第 2 步：前端注册

`src/i18n/index.ts`：

```typescript
import xx from "./locales/xx.json";

export const SUPPORTED_LANGUAGES = {
  // ...
  xx: { code: "xx", name: "Your Language" },
};

const localeFiles: Record<string, Record<string, unknown>> = {
  // ...
  xx,
};
```

### 第 3 步：后端登记

`src-tauri/src/menu.rs` 只需两处改动：

```rust
// 1) 语言列表（数组长度要跟着改），顺序即菜单顺序
pub const LANGS: [&str; 6] = ["zh-SC", "zh-TC", "en", "ja", "ko", "xx"];

// 2) 语言代码 → 语言包
fn locale_json(lang: &str) -> &'static str {
    match lang {
        // ...
        "xx" => include_str!("../../src/i18n/locales/xx.json"),
        _ => include_str!("../../src/i18n/locales/en.json"),
    }
}
```

菜单项 ID 约定为 `lang-<语言代码>`。`src-tauri/src/lib.rs` 的菜单事件分发需要补一行：

```rust
"lang-xx" => switch_lang(app, "xx"),
```

至此完成。勾选状态、菜单文本重建、前端切换都由现有逻辑自动处理。

## 校验

新增语言后建议跑一遍键集合比对，确保没有漏翻（漏掉的键会回退到简体中文）：

```bash
node -e '
const fs=require("fs");
const langs=["zh-SC","zh-TC","en","ja","ko","xx"];
const flat=(o,p="")=>Object.entries(o).flatMap(([k,v])=>typeof v==="object"&&v!==null?flat(v,p+k+"."):[p+k]);
const base=new Set(flat(JSON.parse(fs.readFileSync("src/i18n/locales/zh-SC.json","utf8"))));
for(const l of langs){
  const keys=new Set(flat(JSON.parse(fs.readFileSync(`src/i18n/locales/${l}.json`,"utf8"))));
  const missing=[...base].filter(k=>!keys.has(k));
  console.log(l, "missing:", missing.length ? missing : "none");
}'
```

再执行 `npx tsc --noEmit` 与 `cd src-tauri && cargo check` 确认编译通过。

## 常见问题

**Q: 切换后界面没变化？**
A: 三步都要做完。可以先看 `localStorage["llmperf-language"]` 是否变成了新代码。

**Q: 菜单里没有新语言？**
A: 检查 `LANGS` 数组长度是否同步改了（长度写死会编译报错），以及 `locale_json` 是否加了分支。

**Q: 占位符 `{0}` 没显示数字？**
A: 占位符必须原样保留，翻译时不要改成 `{ 0 }` 或全角括号。

**Q: 可以加任意语言吗？**
A: 可以，只要 JSON 键与 `zh-SC.json` 完全一致。注意 macOS 的「隐藏」类菜单项只在 macOS 上渲染，但文本仍需翻译。

## 文件位置总览

```
src/i18n/
├── index.ts              # 语言包加载、t() 翻译、语言切换与持久化
└── locales/
    ├── zh-SC.json        # 简体中文（键最全，新增语言以它为模板）
    ├── zh-TC.json        # 繁体中文
    ├── en.json           # English
    ├── ja.json           # 日本語
    └── ko.json           # 한국어   ← 新增语言按此格式添加

src-tauri/src/
├── menu.rs               # LANGS + locale_json + 动态菜单构建
└── lib.rs                # 菜单事件 → 通知前端切换
```
