import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
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
}

function App() {
  const [baseURL, setBaseURL] = useState("https://api.openai.com/v1");
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState("gpt-3.5-turbo");
  const [concurrency, setConcurrency] = useState(4);
  const [message, setMessage] = useState("");

  const [windows, setWindows] = useState<ChatWindow[]>(() =>
    Array.from({ length: 4 }, (_, i) => ({
      id: i,
      messages: [],
      status: "idle",
    }))
  );

  const updateWindow = (
    id: number,
    updater: (w: ChatWindow) => ChatWindow
  ) => {
    setWindows((prev) => prev.map((w) => (w.id === id ? updater(w) : w)));
  };

  const addMessage = (windowId: number, msg: ChatMessage) => {
    updateWindow(windowId, (w) => ({
      ...w,
      messages: [...w.messages, msg],
    }));
  };

  const handleSend = async () => {
    if (!message.trim() || !baseURL.trim() || !apiKey.trim()) return;

    // 先更新所有窗口状态为 sending
    setWindows((prev) =>
      prev.map((w, i) =>
        i < concurrency
          ? { ...w, status: "sending" as const, error: undefined }
          : { ...w, status: "idle" as const, messages: [] }
      )
    );

    // 调用 Rust 后端并发请求
    const results = await invoke<
      Array<{
        window_id: number;
        assistant_content: string;
        duration_ms: number;
        error: string | null;
      }>
    >("send_concurrent_request", {
      config: {
        base_url: baseURL,
        api_key: apiKey,
        model: model,
        message: message,
      },
      concurrency,
    });

    // 将 Rust 返回的结果写入各个窗口
    for (const r of results) {
      addMessage(r.window_id, {
        role: "user",
        content: message,
      });

      if (r.error) {
        updateWindow(r.window_id, (w) => ({
          ...w,
          status: "error" as const,
          error: r.error,
          duration: r.duration_ms,
        }));
      } else {
        addMessage(r.window_id, {
          role: "assistant",
          content: r.assistant_content,
        });
        updateWindow(r.window_id, (w) => ({
          ...w,
          status: "done" as const,
          duration: r.duration_ms,
        }));
      }
    }
  };

  const clearAllWindows = () => {
    setWindows((prev) =>
      prev.map((w) => ({
        ...w,
        messages: [],
        status: "idle" as const,
        error: undefined,
        duration: undefined,
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
              placeholder="https://api.openai.com/v1"
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
              placeholder="输入要测试的消息..."
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
      <div className="chat-grid">
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
        return "⏳ 发送中...";
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
        {win.messages.length === 0 && win.status === "idle" && (
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
