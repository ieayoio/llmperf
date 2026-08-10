import { useEffect, useState, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { initI18n, t, getCurrentLanguage, setLanguage } from "./i18n";
import "./App.css";

// 初始化国际化（从 localStorage 加载语言设置）
initI18n();

/** 同步语言设置到后端菜单栏 */
async function syncLanguageToMenu(): Promise<void> {
  const lang = getCurrentLanguage();
  try {
    await invoke<void>("set_initial_language", { lang });
  } catch (e) {
    console.warn("[i18n] 同步语言到菜单失败:", e);
  }
}

/** 单条聊天消息 */
interface ChatMessage {
  role: "user" | "assistant";
  content: string;
  /** 思考/推理内容（推理模型特有） */
  reasoningContent?: string;
}

/** 单个对话窗口的状态 */
interface ChatWindow {
  id: number;
  /** 完整的对话历史（user/assistant 交替） */
  messages: ChatMessage[];
  /** 流式响应状态 */
  status: "idle" | "sending" | "done" | "error";
  error?: string;
  duration?: number;
  /** 补全阶段 token 速度（每秒 token 数） */
  completionTps?: number;
  /** Prompt 阶段 token 速度（每秒 token 数） */
  promptTps?: number;
  /** 流式累积正文内容（尚未成为正式消息） */
  accumulatedContent: string;
  /** 流式累积思考内容（推理模型的 thinking 过程） */
  accumulatedReasoning: string;
  /** 思考结束时间（首个正文 chunk 到达时的 duration_ms），用于计算思考耗时 */
  reasoningEndTime?: number;
  /** 思考阶段耗时（毫秒），即从请求开始到正文内容首次输出的时间 */
  reasoningDuration?: number;
}

/** 流式 chunk 事件 */
interface StreamChunkPayload {
  /** 目标 React 窗口 ID（与 `ChatWindow.id` 对应），前端按此字段严格过滤 */
  target_window_id: number;
  content: string;
  /** 思考/推理内容增量（推理模型特有） */
  reasoning_content?: string;
  finished: boolean;
  error: string | null;
  duration_ms: number;
  /** 补全阶段 token 速度（每秒 token 数，无数据时为 null） */
  completion_tokens_per_second?: number | null;
  /** Prompt 阶段 token 速度（每秒 token 数，无数据时为 null） */
  prompt_tokens_per_second?: number | null;
}

function App() {
  const [baseURL, setBaseURL] = useState("http://127.0.0.1:16777/v1");
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState("gpt-3.5-turbo");
  const [concurrency, setConcurrency] = useState(1);
  const [message, setMessage] = useState("");

  // 当前语言状态（用于触发 UI 重新渲染）
  const [currentLang, setCurrentLang] = useState(getCurrentLanguage());

  // 窗口状态：每个窗口维护自己的对话历史
  const [windows, setWindows] = useState<ChatWindow[]>(() =>
    Array.from({ length: 4 }, (_, i) => ({
      id: i,
      messages: [],
      status: "idle",
      accumulatedContent: "",
      accumulatedReasoning: "",
      reasoningEndTime: undefined,
      completionTps: undefined,
      promptTps: undefined,
    }))
  );

  // 用 ref 始终持有最新的窗口状态，避免 handleSend 中的闭包过时问题
  const windowsRef = useRef<ChatWindow[]>(windows);
  windowsRef.current = windows;

  // 监听 Tauri 原生菜单的语言切换事件
  useEffect(() => {
    const unlisten = listen<string>("language-changed", (event) => {
      setLanguage(event.payload);
      setCurrentLang(getCurrentLanguage());
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // 初始化时同步语言设置到后端菜单栏
  useEffect(() => {
    syncLanguageToMenu();
  }, []);

  // 监听关于对话框事件
  useEffect(() => {
    const unlisten = listen<void>("menu-about", () => {
      alert(`${t('app.title')}\n版本 0.1.0`);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // 监听流式事件
  useEffect(() => {
    let cancelled = false;
    listen<StreamChunkPayload>("stream_chunk", (event) => {
      if (cancelled) return;
      const payload = event.payload;

      // 调试日志：打印完成事件的 token 速度
      if (payload.finished) {
        console.log(`[llmperf] window=${payload.target_window_id} finished duration=${payload.duration_ms} completion_tps=${payload.completion_tokens_per_second} prompt_tps=${payload.prompt_tokens_per_second}`);
      }

      setWindows((prev) =>
        prev.map((w) => {
          // 严格按目标窗口 ID 过滤，忽略其他窗口的事件
          if (w.id !== payload.target_window_id) return w;

          if (payload.finished) {
            // 每个窗口都把 assistant 消息写进自己的 messages 历史。
            // 配合 handleSend 中"每个窗口追加 user 消息"的设计，确保每个窗口
            // 都能完整看到"你 → AI"的对话流（多轮时也各自累积，不会丢历史）。
            const nextMessages = [
              ...w.messages,
              {
                role: "assistant" as const,
                content:
                  w.accumulatedContent ||
                  (payload.error ? `错误: ${payload.error}` : ""),
                ...(w.accumulatedReasoning.length > 0
                  ? { reasoningContent: w.accumulatedReasoning }
                  : {}),
              },
            ];
            return {
              ...w,
              messages: nextMessages,
              status: payload.error ? ("error" as const) : ("done" as const),
              error: payload.error || undefined,
              duration: payload.duration_ms,
              /** 思考耗时：首个正文 chunk 到达时的 duration_ms，无正文则等于总耗时 */
              reasoningDuration: w.reasoningEndTime ?? payload.duration_ms,
              completionTps: payload.completion_tokens_per_second ?? undefined,
              promptTps: payload.prompt_tokens_per_second ?? undefined,
              accumulatedContent: "",
              accumulatedReasoning: "",
              reasoningEndTime: undefined,
            };
          } else {
            // 流式 chunk：分别累积正文和思考内容
            const newAccumulated = w.accumulatedContent + payload.content;
            const newAccumulatedReasoning =
              (w.accumulatedReasoning || "") + (payload.reasoning_content || "");
            // 记录思考结束时间：当正文内容首次出现时，记录当前耗时作为思考结束时间
            const newReasoningEndTime =
              w.reasoningEndTime === undefined && payload.content.length > 0
                ? payload.duration_ms
                : w.reasoningEndTime;
            return {
              ...w,
              accumulatedContent: newAccumulated,
              accumulatedReasoning: newAccumulatedReasoning,
              reasoningEndTime: newReasoningEndTime,
              status: "sending" as const,
            };
          }
        })
      );
    });

    return () => {
      cancelled = true;
    };
  }, []);

  // 并发数变化时同步窗口数组
  useEffect(() => {
    setWindows((prev) => {
      if (prev.length === concurrency) return prev;
      if (prev.length > concurrency) {
        // 缩小：保留前 N 个窗口，去掉多余的
        return prev.slice(0, concurrency);
      }
      // 放大：在末尾追加新的空窗口
      const added = Array.from(
        { length: concurrency - prev.length },
        (_, i) => ({
          id: prev.length + i,
          messages: [],
          status: "idle" as const,
          accumulatedContent: "",
          accumulatedReasoning: "",
          reasoningEndTime: undefined,
          completionTps: undefined,
          promptTps: undefined,
        })
      );
      return [...prev, ...added];
    });
  }, [concurrency]);

  const handleSend = async () => {
    if (!message.trim() || !baseURL.trim() || !apiKey.trim()) return;
    const userMessage = message.trim();

    // 1) 构造发往后端的完整消息：基于 0 号窗口最新 messages 追加本轮 user。
    //    先算出 fullMessages，再 setWindows，避免依赖 setState 异步时机导致漏发 user。
    const baseMessages = windowsRef.current[0]?.messages ?? [];
    const fullMessages = [
      ...baseMessages,
      { role: "user" as const, content: userMessage },
    ];

    // 2) 每个活跃窗口都把 user 消息追加到自己的 messages 历史。
    //    每个窗口都展示完整的"你 → AI"对话轮次；多轮时每窗口各自累积（每轮只追加一次新 user）。
    setWindows((prev) =>
      prev.map((w, i) =>
        i < concurrency
          ? {
              ...w,
              messages: [...w.messages, { role: "user" as const, content: userMessage }],
              status: "sending" as const,
              accumulatedContent: "",
              accumulatedReasoning: "",
              completionTps: undefined,
              promptTps: undefined,
              error: undefined,
            }
          : w
      )
    );

    // 3) 为每个活跃窗口发送一份请求，所有请求使用相同 messages，仅 window_id 不同用于后端分流
    const promises = Array.from({ length: concurrency }).map((_, i) =>
      invoke<void>("send_concurrent_request", {
        config: {
          base_url: baseURL,
          api_key: apiKey,
          model: model,
          messages: fullMessages,
          window_id: i,
        },
        concurrency: 1,
      })
    );

    await Promise.all(promises);
    setMessage("");
    // 状态变更由 stream_chunk 事件回调统一处理
  };

  const clearAllWindows = () => {
    setWindows((prev) =>
      prev.map((w) => ({
        ...w,
        messages: [],
        status: "idle" as const,
        error: undefined,
        duration: undefined,
        completionTps: undefined,
        promptTps: undefined,
        accumulatedContent: "",
        accumulatedReasoning: "",
        reasoningEndTime: undefined,
      }))
    );
  };

  return (
    <div className="app-container">
      {/* 使用 currentLang 触发语言切换时的重新渲染 */}
      <div style={{ display: "none" }} aria-hidden="true" key={currentLang}></div>

      {/* ====== 顶部配置栏 ====== */}
      <div className="config-panel">
        <h1 className="app-title">{t("app.title")}</h1>

        <div className="config-row">
          <div className="config-field">
            <label>{t("config.baseURL")}</label>
            <input
              type="text"
              placeholder={t("config.baseURLPlaceholder")}
              value={baseURL}
              onChange={(e) => setBaseURL(e.target.value)}
            />
          </div>

          <div className="config-field">
            <label>{t("config.apiKey")}</label>
            <input
              type="password"
              placeholder={t("config.apiKeyPlaceholder")}
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
            />
          </div>

          <div className="config-field">
            <label>{t("config.model")}</label>
            <input
              type="text"
              placeholder={t("config.modelPlaceholder")}
              value={model}
              onChange={(e) => setModel(e.target.value)}
            />
          </div>
        </div>

        <div className="config-row">
          <div className="config-field">
            <label>{t("config.concurrency")}</label>
            <input
              type="number"
              min={1}
              max={50}
              value={concurrency}
              // 将输入强制限制在 [1, 50]：空值、负数、超过 50 都会被 clamp 修正
              onChange={(e) => {
                const raw = e.target.value;
                if (raw === "") {
                  // 输入框为空时先置 1（允许用户清空后再输入），避免出现 0 / NaN
                  setConcurrency(1);
                  return;
                }
                const n = Number(raw);
                if (Number.isNaN(n)) return;
                setConcurrency(Math.min(50, Math.max(1, Math.floor(n))));
              }}
            />
          </div>

          <div className="config-field flex-1">
            <label>{t("config.sendMessage")}</label>
            <input
              type="text"
              placeholder={t("config.messagePlaceholder")}
              value={message}
              onChange={(e) => setMessage(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") handleSend();
              }}
            />
          </div>

          <div className="config-actions">
            <button className="btn btn-primary" onClick={handleSend}>
              {t("config.send")}
            </button>
            <button className="btn btn-secondary" onClick={clearAllWindows}>
              {t("config.clear")}
            </button>
          </div>
        </div>
      </div>

      {/* ====== 底部聊天窗口区域 ====== */}
      <div className={`chat-grid cols-${concurrency}`}>
        {windows.map((win) => (
          <ChatWindow key={win.id} window={win} />
        ))}
      </div>
    </div>
  );
}

/* ====== 单个聊天窗口组件 ====== */
function ChatWindow({ window: win }: { window: ChatWindow }) {
  const statusLabel = () => {
    const contentLen = win.accumulatedContent.length;
    const reasoningLen = win.accumulatedReasoning.length;
    // 格式化工具：保留两位小数，去掉无意义的末尾零
    const fmtTps = (tps?: number, label?: string) =>
      tps !== undefined && tps > 0 ? ` ${label} ${tps.toFixed(2)} tok/s` : "";
    switch (win.status) {
      case "idle":
        return t("status.idle");
      case "sending":
        if (reasoningLen > 0) {
          return t("status.sending_with_reasoning", contentLen, reasoningLen);
        }
        return t("status.sending", contentLen);
      case "done":
        return t("status.done", win.duration ?? 0, fmtTps(win.completionTps, t("status.completion_label")), fmtTps(win.promptTps, t("status.prompt_label")));
      case "error":
        return t("status.error", win.duration ?? 0, fmtTps(win.completionTps, t("status.completion_label")), fmtTps(win.promptTps, t("status.prompt_label")));
    }
  };

  /** 格式化时间：毫秒 → 秒（保留1位小数）或毫秒 */
  const formatDuration = (ms?: number): string => {
    if (ms === undefined || ms <= 0) return "";
    if (ms >= 1000) {
      return `${(ms / 1000).toFixed(1)}s`;
    }
    return `${ms}ms`;
  };

  return (
    <div className={`chat-window status-${win.status}`}>
      <div className="chat-header">
        <span className="chat-window-title">{t("chat.window", win.id + 1)}</span>
        <span className="chat-status">{statusLabel()}</span>
      </div>

      <div className="chat-messages">
        {win.messages.length === 0 && win.accumulatedContent.length === 0 && win.status === "idle" && (
          <div className="chat-empty">{t("chat.empty")}</div>
        )}
        {win.messages.map((msg, i) => (
          <div
            key={i}
            className={`chat-msg ${msg.role === "user" ? "user" : "assistant"}`}
          >
            <div className="chat-msg-label">
              {msg.role === "user" ? t("chat.user") : t("chat.assistant")}
            </div>
            {/* 已完成的思考过程（显示在正文上方） */}
            {msg.reasoningContent && (
              <details className="reasoning-details">
                <summary className="reasoning-summary">
                  <span>{t("chat.reasoningSummary")}</span>
                  <span className="reasoning-duration"> · {formatDuration(win.reasoningDuration)}</span>
                </summary>
                <div className="reasoning-content-text">{msg.reasoningContent}</div>
              </details>
            )}
            <div className="chat-msg-content">{msg.content}</div>
          </div>
        ))}
        {/* 流式输出中的思考过程（推理模型的 thinking，显示在正文上方） */}
        {win.status === "sending" && win.accumulatedReasoning.length > 0 && (
          <div className="chat-msg assistant reasoning-streaming">
            <div className="chat-msg-label">{t("chat.reasoning")}</div>
            <div className="chat-msg-content reasoning-content">
              {win.accumulatedReasoning}
              <span className="streaming-cursor">▌</span>
            </div>
          </div>
        )}
        {/* 流式输出中的累积内容（尚未成为正式消息）。
            每个窗口流式时都展示 accumulatedContent，finished 后 assistant 会被写入 messages，
            且 accumulatedContent 会被清空，渲染自动切换到 messages 模式 */}
        {win.status === "sending" && win.accumulatedContent.length > 0 && (
          <div className="chat-msg assistant streaming">
            <div className="chat-msg-label">{t("chat.assistant")}</div>
            <div className="chat-msg-content">
              {win.accumulatedContent}
              <span className="streaming-cursor">▌</span>
            </div>
          </div>
        )}
        {win.status === "error" && win.error && (
          <div className="chat-error">
            <strong>{t("chat.error")}</strong> {win.error}
          </div>
        )}
      </div>
    </div>
  );
}

export default App;
