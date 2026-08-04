import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

interface ChatMessage {
  role: "user" | "assistant";
  content: string;
}

interface ChatWindow {
  id: number;
  messages: ChatMessage[];
  status: "idle" | "sending" | "done" | "error";
  error?: string;
  duration?: number;
  accumulatedContent: string; // 流式累积内容
}

interface StreamChunkPayload {
  window_id: number;
  content: string;
  finished: boolean;
  error: string | null;
  duration_ms: number;
}

function App() {
  const [baseURL, setBaseURL] = useState("http://127.0.0.1:16777/v1");
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState("gpt-3.5-turbo");
  const [concurrency, setConcurrency] = useState(1);
  const [message, setMessage] = useState("");

  // 窗口数量跟随 concurrency 同步
  const [windows, setWindows] = useState<ChatWindow[]>(() =>
    Array.from({ length: 4 }, (_, i) => ({
      id: i,
      messages: [],
      status: "idle",
      accumulatedContent: "",
    }))
  );

  // 监听流式事件
  useEffect(() => {
    let cancelled = false;
    listen<StreamChunkPayload>("stream_chunk", (event) => {
      if (cancelled) return;
      const payload = event.payload;

      setWindows((prev) =>
        prev.map((w) => {
          if (w.id !== payload.window_id) return w;

          if (payload.finished) {
            // 最终完成
            const finalMsg: ChatMessage = {
              role: "assistant",
              content: w.accumulatedContent || (payload.error ? `错误: ${payload.error}` : ""),
            };
            return {
              ...w,
              messages: [...w.messages, finalMsg],
              status: payload.error ? ("error" as const) : ("done" as const),
              error: payload.error || undefined,
              duration: payload.duration_ms,
              accumulatedContent: "",
            };
          } else {
            // 流式 chunk，追加到累积内容
            const newAccumulated = w.accumulatedContent + payload.content;
            // 更新最后一条 assistant 消息（如果存在）
            let realIndex = -1;
            for (let i = w.messages.length - 1; i >= 0; i--) {
              if (w.messages[i].role === "assistant") {
                realIndex = i;
                break;
              }
            }
            let newMessages = w.messages;
            if (realIndex >= 0) {
              newMessages = [...w.messages];
              newMessages[realIndex] = {
                ...newMessages[realIndex],
                content: newAccumulated,
              };
            }
            return {
              ...w,
              accumulatedContent: newAccumulated,
              messages: newMessages,
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
        })
      );
      return [...prev, ...added];
    });
  }, [concurrency]);

  const handleSend = async () => {
    if (!message.trim() || !baseURL.trim() || !apiKey.trim()) return;

    // 先清空所有窗口并更新状态为 sending
    setWindows((prev) =>
      prev.map((w, i) =>
        i < concurrency
          ? { ...w, status: "sending" as const, error: undefined, messages: [], accumulatedContent: "" }
          : { ...w, status: "idle" as const, messages: [], accumulatedContent: "" }
      )
    );

    // 调用 Rust 后端并发流式请求
    await invoke<void>("send_concurrent_request", {
      config: {
        base_url: baseURL,
        api_key: apiKey,
        model: model,
        message: message,
      },
      concurrency,
    });
  };

  const clearAllWindows = () => {
    setWindows((prev) =>
      prev.map((w) => ({
        ...w,
        messages: [],
        status: "idle" as const,
        error: undefined,
        duration: undefined,
        accumulatedContent: "",
      }))
    );
  };

  return (
    <div className="app-container">
      {/* ====== 顶部配置栏 ====== */}
      <div className="config-panel">
        <h1 className="app-title">⚡ LLM 并发测试工具</h1>

        <div className="config-row">
          <div className="config-field">
            <label>Base URL</label>
            <input
              type="text"
              placeholder="http://127.0.0.1:16777/v1"
              value={baseURL}
              onChange={(e) => setBaseURL(e.target.value)}
            />
          </div>

          <div className="config-field">
            <label>API Key</label>
            <input
              type="password"
              placeholder="sk-..."
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
            />
          </div>

          <div className="config-field">
            <label>Model</label>
            <input
              type="text"
              placeholder="gpt-3.5-turbo"
              value={model}
              onChange={(e) => setModel(e.target.value)}
            />
          </div>
        </div>

        <div className="config-row">
          <div className="config-field">
            <label>并发数</label>
            <input
              type="number"
              min={1}
              max={50}
              value={concurrency}
              onChange={(e) => setConcurrency(Number(e.target.value))}
            />
          </div>

          <div className="config-field flex-1">
            <label>发送消息</label>
            <input
              type="text"
              placeholder="输入要测试的消息 (Enter 发送)"
              value={message}
              onChange={(e) => setMessage(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") handleSend();
              }}
            />
          </div>

          <div className="config-actions">
            <button className="btn btn-primary" onClick={handleSend}>
              🚀 发送
            </button>
            <button className="btn btn-secondary" onClick={clearAllWindows}>
              🗑 清空
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
    switch (win.status) {
      case "idle":
        return "⏸ 空闲";
      case "sending":
        return `⏳ 发送中... (${win.accumulatedContent.length} 字符)`;
      case "done":
        return `✅ 完成 (${win.duration}ms)`;
      case "error":
        return `❌ ${win.duration ?? 0}ms`;
    }
  };

  return (
    <div className={`chat-window status-${win.status}`}>
      <div className="chat-header">
        <span className="chat-window-title">窗口 {win.id + 1}</span>
        <span className="chat-status">{statusLabel()}</span>
      </div>

      <div className="chat-messages">
        {win.messages.length === 0 && win.accumulatedContent.length === 0 && win.status === "idle" && (
          <div className="chat-empty">等待发送...</div>
        )}
        {win.messages.map((msg, i) => (
          <div
            key={i}
            className={`chat-msg ${msg.role === "user" ? "user" : "assistant"}`}
          >
            <div className="chat-msg-label">
              {msg.role === "user" ? "👤 你" : "🤖 模型"}
            </div>
            <div className="chat-msg-content">{msg.content}</div>
          </div>
        ))}
        {/* 流式输出中的累积内容（尚未成为正式消息） */}
        {win.status === "sending" && win.accumulatedContent.length > 0 && (
          <div className="chat-msg assistant streaming">
            <div className="chat-msg-label">🤖 模型</div>
            <div className="chat-msg-content">
              {win.accumulatedContent}
              <span className="streaming-cursor">▌</span>
            </div>
          </div>
        )}
        {win.status === "error" && win.error && (
          <div className="chat-error">
            <strong>错误:</strong> {win.error}
          </div>
        )}
      </div>
    </div>
  );
}

export default App;
