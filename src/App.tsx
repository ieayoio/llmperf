import { useEffect, useState, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { initI18n, t, getCurrentLanguage, setLanguage } from "./i18n";
import "./App.css";

// 初始化国际化（从 localStorage 加载语言设置）
initI18n();

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
}

/** 流式 chunk 事件 */
interface StreamChunkPayload {
  window_id: number;
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

  // 监听流式事件
  useEffect(() => {
    let cancelled = false;
    listen<StreamChunkPayload>("stream_chunk", (event) => {
      if (cancelled) return;
      const payload = event.payload;

      // 调试日志：打印完成事件的 token 速度
      if (payload.finished) {
        console.log(`[llmperf] window=${payload.window_id} finished duration=${payload.duration_ms} completion_tps=${payload.completion_tokens_per_second} prompt_tps=${payload.prompt_tokens_per_second}`);
      }

      setWindows((prev) =>
        prev.map((w) => {
          if (w.id !== payload.window_id) return w;

          if (payload.finished) {
            // 最终完成：追加 assistant 消息（正文 + 思考内容）
            const assistantMsg: ChatMessage = {
              role: "assistant",
              content: w.accumulatedContent || (payload.error ? `错误: ${payload.error}` : ""),
            };
            // 如果有思考内容，也保存到消息中
            if (w.accumulatedReasoning.length > 0) {
              assistantMsg.reasoningContent = w.accumulatedReasoning;
            }
            return {
              ...w,
              messages: [...w.messages, assistantMsg],
              status: payload.error ? ("error" as const) : ("done" as const),
              error: payload.error || undefined,
              duration: payload.duration_ms,
              completionTps: payload.completion_tokens_per_second ?? undefined,
              promptTps: payload.prompt_tokens_per_second ?? undefined,
              accumulatedContent: "",
              accumulatedReasoning: "",
            };
          } else {
            // 流式 chunk：分别累积正文和思考内容
            const newAccumulated = w.accumulatedContent + payload.content;
            const newAccumulatedReasoning =
              (w.accumulatedReasoning || "") + (payload.reasoning_content || "");
            return {
              ...w,
              accumulatedContent: newAccumulated,
              accumulatedReasoning: newAccumulatedReasoning,
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

    // 使用函数式更新，获取最新状态后再追加用户消息并发送请求
    const activeWindows = await new Promise<ChatWindow[]>((resolve) => {
      setWindows((prev) => {
        const updated = prev.map((w, i) =>
          i < concurrency
            ? { ...w, messages: [...w.messages, { role: "user" as const, content: userMessage }], status: "sending" as const, accumulatedContent: "", accumulatedReasoning: "", completionTps: undefined, promptTps: undefined }
            : w
        );
        const active = updated.filter((w) => w.id < concurrency);
        windowsRef.current = updated;
        resolve(active);
        return updated;
      });
    });

    // 为每个活跃的窗口发送请求
    const promises = activeWindows.map((w) =>
      invoke<void>("send_concurrent_request", {
        config: {
          base_url: baseURL,
          api_key: apiKey,
          model: model,
          messages: w.messages,
          window_id: w.id,
        },
        concurrency: 1,
      })
    );

    await Promise.all(promises);
    setMessage("");
    // 不在这里更新状态，让 listen 回调处理所有状态变更
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
              onChange={(e) => setConcurrency(Number(e.target.value))}
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
                <summary className="reasoning-summary">{t("chat.reasoningSummary")}</summary>
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
        {/* 流式输出中的累积内容（尚未成为正式消息） */}
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
