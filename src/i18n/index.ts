/**
 * 国际化（i18n）工具模块
 * 
 * 功能：
 * - 加载指定语言包
 * - 翻译文本（支持 {0}, {1} 等变量占位符）
 * - 切换语言并持久化到 localStorage
 * 
 * 添加新语言的步骤（无需修改代码逻辑）：
 * 1. 在 locales/ 目录下新建语言文件，如 ja.json
 * 2. 按照 zh.json 的格式翻译所有 value
 * 3. 在本文件的 SUPPORTED_LANGUAGES 中注册新语言
 * 
 * 示例：
 *   import { t, currentLanguage, setLanguage } from '@/i18n';
 *   // 翻译文本
 *   const title = t('app.title');
 *   // 带变量
 *   const text = t('status.sending', 100, true);
 *   // 切换语言
 *   setLanguage('en');
 */

import zh from "./locales/zh.json";
import en from "./locales/en.json";

/** 支持的语言列表 - 非程序员只需在这里添加新语言即可 */
export const SUPPORTED_LANGUAGES: Record<string, { code: string; name: string }> = {
  zh: { code: "zh", name: "中文" },
  en: { code: "en", name: "English" },
  // 添加新语言示例:
  // ja: { code: "ja", name: "日本語" },
  // ko: { code: "ko", name: "한국어" },
  // fr: { code: "fr", name: "Français" },
};

/** 语言包字典 */
const localeFiles: Record<string, Record<string, unknown>> = {
  zh,
  en,
};

/** 当前语言代码，默认为中文 */
let currentLang: string = "zh";

/** 从 localStorage 读取上次选择的语言 */
function loadSavedLanguage(): string {
  try {
    const saved = localStorage.getItem("llmperf-language");
    if (saved && saved in localeFiles) {
      return saved;
    }
  } catch {
    // localStorage 不可用时忽略
  }
  return "zh";
}

/**
 * 递归地将嵌套对象展平为以点号分隔的路径
 * 例如: { app: { title: "Hi" } } → { "app.title": "Hi" }
 */
function flatten(obj: Record<string, unknown>, prefix = ""): Record<string, string> {
  const result: Record<string, string> = {};
  for (const [key, value] of Object.entries(obj)) {
    const fullKey = prefix ? `${prefix}.${key}` : key;
    if (typeof value === "object" && value !== null && !Array.isArray(value)) {
      Object.assign(result, flatten(value as Record<string, unknown>, fullKey));
    } else {
      result[fullKey] = String(value);
    }
  }
  return result;
}

/** 已展平的各语言翻译字典 */
const flatLocales: Record<string, Record<string, string>> = {};
for (const [code, locale] of Object.entries(localeFiles)) {
  flatLocales[code] = flatten(locale as Record<string, unknown>);
}

/**
 * 翻译函数
 * 
 * @param key - 翻译键，使用点号分隔的路径，如 "app.title"
 * @param replacements - 可选的变量替换值，对应文本中的 {0}, {1} 等占位符
 * @returns 翻译后的文本
 * 
 * @example
 *   t('app.title')                    // → "⚡ LLM 并发测试工具"
 *   t('status.sending', 100, true)    // → "⏳ 发送中... (正文 100 字符, 思考 50 字符)"
 */
export function t(key: string, ...replacements: (string | number | boolean)[]): string {
  const locale = flatLocales[currentLang] || flatLocales["zh"];
  let text = locale[key] || locale[`zh.${key}`] || key; // fallback 到中文，再 fallback 到 key 本身

  // 替换 {0}, {1} 等占位符
  replacements.forEach((value, index) => {
    text = text.replace(new RegExp(`\\{${index}\\}`, "g"), String(value));
  });

  return text;
}

/** 获取当前语言代码 */
export function getCurrentLanguage(): string {
  return currentLang;
}

/** 切换语言
 * 
 * @param lang - 语言代码，如 "zh"、"en"
 * 
 * 切换后会自动：
 * - 保存到 localStorage，下次打开应用时保持选择
 * - 派发自定义 DOM 事件通知 React 组件重新渲染
 */
export function setLanguage(lang: string): void {
  if (!(lang in localeFiles)) {
    console.warn(`[i18n] 语言 "${lang}" 不受支持，当前使用: ${currentLang}`);
    return;
  }
  currentLang = lang;
  try {
    localStorage.setItem("llmperf-language", lang);
  } catch {
    // ignore
  }
  // 派发 DOM 事件通知 React 组件重新渲染
  window.dispatchEvent(new Event("llmperf-language-changed"));
}

/** 初始化：从 localStorage 加载语言设置，并监听 Tauri 原生菜单的切换事件 */
export function initI18n(): void {
  currentLang = loadSavedLanguage();
}
