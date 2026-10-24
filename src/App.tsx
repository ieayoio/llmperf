import { useEffect, useState, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { initI18n, t, getCurrentLanguage, setLanguage } from "./i18n";
import {
  buildCsv,
  buildExportFilename,
  buildSweepCsv,
  parseLevels,
  parseOptionalNumber,
  summarizeRun,
  type ItlStats,
  type RunSummary,
  type SingleResult,
  type Stats,
  type SweepPoint,
} from "./metrics";
import { SweepPanel, type SweepProgress } from "./SweepPanel";
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
  /** 该条回复的思考耗时（毫秒），随消息保存，避免历史消息共用同一个值 */
  reasoningDuration?: number;
}

/** 单个对话窗口的状态 */
interface ChatWindow {
  id: number;
  /** 完整的对话历史（user/assistant 交替） */
  messages: ChatMessage[];
  /** 流式响应状态 */
  status: "idle" | "sending" | "done" | "error" | "canceled";
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
  /** 首 token（思考或正文）延迟（毫秒） */
  ttft?: number;
  /** 正文首字延迟（毫秒）：推理模型下明显大于 ttft */
  firstContent?: number;
  /** 平均分片间隔（毫秒/分片），近似 TPOT */
  tpot?: number;
  /** 分片间隔 P95（毫秒），用于看抖动 */
  itlP95?: number;
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
  /** 首 token 延迟（毫秒），仅完成事件携带 */
  ttft_ms?: number | null;
  /** 正文首字延迟（毫秒），仅完成事件携带 */
  first_content_ms?: number | null;
  /** 平均分片间隔（毫秒/分片），仅完成事件携带 */
  tpot_ms?: number | null;
  /** 分片间隔分位数统计，仅完成事件携带 */
  itl?: ItlStats | null;
  /** 是否被用户主动取消（后端结构化字段，不依赖 error 文案） */
  canceled?: boolean;
  /** 补全阶段 token 速度（每秒 token 数，无数据时为 null） */
  completion_tokens_per_second?: number | null;
  /** Prompt 阶段 token 速度（每秒 token 数，无数据时为 null） */
  prompt_tokens_per_second?: number | null;
}

/** 梯度扫描保留的最大并发档位数（与 parseLevels 的上限保持一致） */
const MAX_SWEEP_LEVELS = 8;

/**
 * 是否为 macOS
 *
 * 菜单快捷键统一用 `CmdOrCtrl` 描述，展示时需要展开成平台对应的按键名。
 */
const IS_MAC = navigator.userAgent.includes("Mac");

/** 「快捷键一览」中的一行（来自后端菜单定义） */
interface MenuShortcut {
  /** 菜单项 ID */
  id: string;
  /** 语言包 menu 区块下的 key，用于取显示名 */
  key: string;
  /** 后端快捷键字符串，如 "CmdOrCtrl+E" */
  accel: string;
}

/**
 * 把后端快捷键字符串拆成按键列表
 *
 * `CmdOrCtrl` 按平台展开成 ⌘ / Ctrl，并把多字符主键统一大写，视觉上更接近键帽。
 */
function formatAccel(accel: string): string[] {
  const parts = accel.split("+").map((part) => {
    switch (part) {
      case "CmdOrCtrl":
        return IS_MAC ? "⌘" : "Ctrl";
      case "Ctrl":
        return IS_MAC ? "⌃" : "Ctrl";
      case "Shift":
        return IS_MAC ? "⇧" : "Shift";
      case "Alt":
        return IS_MAC ? "⌥" : "Alt";
      default:
        return part;
    }
  });
  const mainKey = parts.pop() ?? "";
  return [...parts, mainKey.length > 1 ? mainKey.toUpperCase() : mainKey];
}

/**
 * 扫描模式使用的 window_id 基址
 *
 * 与对话窗口（0 起）隔离，避免互相覆盖后端取消注册表里的 token，
 * 也让「全部取消」能同时覆盖两边的请求。
 */
const SWEEP_WINDOW_BASE = 100_000;

function App() {
  const [baseURL, setBaseURL] = useState("http://127.0.0.1:16777/v1");
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState("gpt-3.5-turbo");
  const [concurrency, setConcurrency] = useState(1);
  const [message, setMessage] = useState("");

  // 采样参数：留空表示使用服务端默认值
  // 压测时建议固定 max tokens（否则不同轮次的 tok/s 无法横向比较）
  const [maxTokens, setMaxTokens] = useState("");
  const [temperature, setTemperature] = useState("");

  // 最近一轮的汇总结果（null 表示还没完成过一轮）
  const [lastRun, setLastRun] = useState<RunSummary | null>(null);
  // 导出提示：成功时为文件路径，失败时为错误原因
  const [exportHint, setExportHint] = useState("");

  // ====== 梯度扫描状态 ======
  // 模式：对话（逐窗口看流式输出）/ 扫描（静默逐档施压，只看指标）
  const [mode, setMode] = useState<"chat" | "sweep">("chat");
  // 并发梯度（逗号分隔）与每档轮次
  const [sweepLevels, setSweepLevels] = useState("1,2,4,8,16");
  const [sweepRounds, setSweepRounds] = useState(1);
  // 已完成的档位结果（逐档追加，扫描中也能实时看图）
  const [sweepPoints, setSweepPoints] = useState<SweepPoint[]>([]);
  const [sweepRunning, setSweepRunning] = useState(false);
  const [sweepProgress, setSweepProgress] = useState<SweepProgress | null>(null);
  const [sweepError, setSweepError] = useState("");
  const [sweepExportHint, setSweepExportHint] = useState("");
  /**
   * 停止标记
   *
   * 仅靠“全部取消”不够：已完成的请求不会变成 canceled，
   * 若只看结果状态就无法判断用户是否按下了停止，扫描会继续跑完剩下的档位。
   */
  const sweepStopRef = useRef(false);

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

  // 监听关于对话框事件：弹出应用内模态框（不用 alert()，避免 Tauri WebView 在
  // 系统对话框上追加 dev 服务器 URL，导致即使 release 也会显示 localhost 行）
  const [showAbout, setShowAbout] = useState(false);
  // 快捷键一览弹窗：行数据由后端菜单定义提供，避免前后端各写一份快捷键表
  const [showShortcuts, setShowShortcuts] = useState(false);
  const [shortcutRows, setShortcutRows] = useState<MenuShortcut[]>([]);
  useEffect(() => {
    const unlisten = listen<void>("menu-about", () => {
      setShowAbout(true);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // 读取「快捷键一览」数据（后端是快捷键表的唯一来源）
  useEffect(() => {
    invoke<MenuShortcut[]>("get_menu_shortcuts")
      .then(setShortcutRows)
      .catch((e) => console.warn("[llmperf] 读取快捷键失败:", e));
  }, []);

  // 监听流式事件
  useEffect(() => {
    let cancelled = false;
    // listen 返回 Promise<UnlistenFn>，必须在 cleanup 里真正取消监听，
    // 否则每次挂载（含 StrictMode 双挂载）都会残留一个监听器
    let unlisten: (() => void) | undefined;

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
            // 取消时也保留已流出的部分内容（accumulatedContent）作为可见输出。
            //
            // 注意：没有任何输出时不写入消息——错误/取消提示由下方状态条展示，
            // 若把"错误: xxx"当成 assistant 内容写进历史，会被当成上下文发给模型。
            const hasOutput =
              w.accumulatedContent.length > 0 || w.accumulatedReasoning.length > 0;
            const nextMessages = hasOutput
              ? [
                  ...w.messages,
                  {
                    role: "assistant" as const,
                    content: w.accumulatedContent,
                    ...(w.accumulatedReasoning.length > 0
                      ? { reasoningContent: w.accumulatedReasoning }
                      : {}),
                    /** 思考耗时：首个正文 chunk 到达时的 duration_ms，无正文则等于总耗时 */
                    reasoningDuration: w.reasoningEndTime ?? payload.duration_ms,
                  },
                ]
              : w.messages;
            // 区分"取消"与"真正错误"：以后端结构化字段为准，不比较文案
            const isCanceled = payload.canceled === true;
            return {
              ...w,
              messages: nextMessages,
              status: payload.error
                ? isCanceled
                  ? ("canceled" as const)
                  : ("error" as const)
                : ("done" as const),
              error: payload.error || undefined,
              duration: payload.duration_ms,
              // 取消时清空 token 速度与各项延迟，避免显示误导性数字
              ttft: isCanceled ? undefined : payload.ttft_ms ?? undefined,
              firstContent: isCanceled ? undefined : payload.first_content_ms ?? undefined,
              tpot: isCanceled ? undefined : payload.tpot_ms ?? undefined,
              itlP95: isCanceled ? undefined : payload.itl?.p95_ms ?? undefined,
              completionTps: isCanceled
                ? undefined
                : payload.completion_tokens_per_second ?? undefined,
              promptTps: isCanceled
                ? undefined
                : payload.prompt_tokens_per_second ?? undefined,
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
    }).then((fn) => {
      // 若组件已卸载则立即取消，避免残留监听
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      unlisten?.();
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
    // 防重复提交：上一轮流式尚未结束时（按钮/回车都可能触发）直接忽略。
    // 否则会重复追加 user 消息，并让后端覆盖注册导致上一轮被取消、半截内容写进历史。
    if (windowsRef.current.some((w) => w.status === "sending")) return;
    // 扫描进行中不接对话请求：两股负载同时压服务器会让两边数据都不可信
    if (sweepRunning) return;
    if (!message.trim() || !baseURL.trim() || !apiKey.trim()) return;
    const userMessage = message.trim();
    // 立刻清空输入框：既避免重复提交，也避免流式结束后清掉用户新输入的内容
    setMessage("");

    // 1) 构造发往后端的完整消息：基于 0 号窗口最新 messages 追加本轮 user。
    //    先算出 fullMessages，再 setWindows，避免依赖 setState 异步时机导致漏发 user。
    //    透传 assistant 消息的 reasoningContent：部分推理模型（如 deepseek-reasoner）
    //    在多轮对话时要求携带上一轮 assistant 的推理链，否则会校验失败或答复降级。
    const baseMessages = windowsRef.current[0]?.messages ?? [];
    const fullMessages = [
      ...baseMessages.map((m) => ({
        role: m.role,
        content: m.content,
        ...(m.reasoningContent && m.reasoningContent.length > 0
          ? { reasoningContent: m.reasoningContent }
          : {}),
      })),
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
              reasoningEndTime: undefined,
              completionTps: undefined,
              promptTps: undefined,
              // 清空上一轮的耗时，避免发送中仍显示旧值
              duration: undefined,
              ttft: undefined,
              firstContent: undefined,
              tpot: undefined,
              itlP95: undefined,
              error: undefined,
            }
          : w
      )
    );

    // 3) 为每个活跃窗口发送一份请求，所有请求使用相同 messages，仅 window_id 不同用于后端分流
    const startedAt = performance.now();
    const maxTokensValue = parseOptionalNumber(maxTokens);
    const promises = Array.from({ length: concurrency }).map((_, i) =>
      invoke<SingleResult[]>("send_concurrent_request", {
        config: {
          base_url: baseURL,
          api_key: apiKey,
          model: model,
          messages: fullMessages,
          window_id: i,
          // 采样参数：null 表示不传，交给服务端默认值
          max_tokens:
            maxTokensValue === null ? null : Math.max(1, Math.round(maxTokensValue)),
          temperature: parseOptionalNumber(temperature),
        },
        concurrency: 1,
      })
    );

    try {
      const responses = await Promise.all(promises);
      // 每个 invoke 返回该窗口的结果数组，展平后得到本轮全部明细
      const results = responses.reduce<SingleResult[]>((acc, r) => acc.concat(r), []);
      if (results.length > 0) {
        setLastRun(summarizeRun(results, performance.now() - startedAt));
      }
    } catch (e) {
      // 命令本身 reject（后端 panic、state 缺失等）时不会有任何流式事件到达，
      // 这里兜底把仍处于 sending 的窗口置为错误态，避免界面永久停在转圈状态
      setWindows((prev) =>
        prev.map((w) =>
          w.status === "sending"
            ? { ...w, status: "error" as const, error: String(e) }
            : w
        )
      );
    }
    // 状态变更由 stream_chunk 事件回调统一处理
  };

  const clearAllWindows = () => {
    // 清空对话的同时丢弃上一轮汇总，避免面板展示与窗口内容不一致
    setLastRun(null);
    setExportHint("");
    setWindows((prev) =>
      prev.map((w) => ({
        ...w,
        messages: [],
        status: "idle" as const,
        error: undefined,
        duration: undefined,
        completionTps: undefined,
        promptTps: undefined,
        ttft: undefined,
        firstContent: undefined,
        tpot: undefined,
        itlP95: undefined,
        accumulatedContent: "",
        accumulatedReasoning: "",
        reasoningEndTime: undefined,
      }))
    );
  };

  /** 清空梯度扫描的结果与提示 */
  const clearSweepResults = () => {
    setSweepPoints([]);
    setSweepError("");
    setSweepExportHint("");
  };

  /** 取消指定窗口的后端请求；后端会发出 finished + 用户已取消 事件，状态由事件统一收敛 */
  const cancelWindow = async (windowId: number) => {
    try {
      await invoke<boolean>("cancel_request", { windowId });
    } catch (e) {
      console.warn(`[llmperf] 取消窗口 ${windowId} 失败:`, e);
    }
  };

  /** 取消所有正在执行的后端请求（整页一键停止） */
  const cancelAll = async () => {
    try {
      await invoke<number[]>("cancel_all_requests");
    } catch (e) {
      console.warn("[llmperf] 全部取消失败:", e);
    }
  };

  /** 把最近一轮逐窗口明细导出为 CSV（写入系统下载目录） */
  const exportCsv = async () => {
    if (!lastRun) return;
    try {
      // 加 BOM，让 Excel 正确识别 UTF-8（错误信息里可能有中文）
      const path = await invoke<string>("save_text_file", {
        filename: buildExportFilename("llmperf"),
        content: "\uFEFF" + buildCsv(lastRun.results),
      });
      setExportHint(t("summary.exported", path));
    } catch (e) {
      setExportHint(t("summary.exportFailed", String(e)));
    }
  };

  /**
   * 执行并发梯度扫描
   *
   * 逐档串行：上一档全部结束后才发下一档，避免不同档位的样本互相干扰。
   * 每路单独 invoke（concurrency=1）是为了拿到准确的完成进度，
   * 后端配合 `silent` 不推送流式事件，结果只从返回值取。
   */
  const runSweep = async () => {
    if (sweepRunning || anySending) return;
    const levels = parseLevels(sweepLevels, MAX_SWEEP_LEVELS);
    const userMessage = message.trim();
    if (levels.length === 0 || !userMessage || !baseURL.trim() || !apiKey.trim()) {
      setSweepError(t("sweep.invalidConfig"));
      return;
    }
    setSweepError("");
    setSweepExportHint("");
    setSweepPoints([]);
    setSweepRunning(true);
    sweepStopRef.current = false;

    const maxTokensValue = parseOptionalNumber(maxTokens);
    const temperatureValue = parseOptionalNumber(temperature);
    const rounds = Math.min(5, Math.max(1, Math.floor(sweepRounds) || 1));
    const collected: SweepPoint[] = [];

    try {
      for (const level of levels) {
        // 用户已点停止：不再进入新的档位
        if (sweepStopRef.current) break;
        const results: SingleResult[] = [];
        let wallClockMs = 0;

        for (let round = 0; round < rounds; round++) {
          if (sweepStopRef.current) break;
          setSweepProgress({
            concurrency: level,
            done: 0,
            total: level,
            round: round + 1,
            rounds,
          });
          const startedAt = performance.now();
          const jobs = Array.from({ length: level }, (_, i) =>
            invoke<SingleResult[]>("send_concurrent_request", {
              config: {
                base_url: baseURL,
                api_key: apiKey,
                model,
                // 扫描只看指标，不带多轮上下文，保证每档的 prompt 一致
                messages: [{ role: "user", content: userMessage }],
                window_id: SWEEP_WINDOW_BASE + i,
                max_tokens:
                  maxTokensValue === null ? null : Math.max(1, Math.round(maxTokensValue)),
                temperature: temperatureValue,
                // 静默模式：不推送逐 token 事件，避免几十路并发刷爆界面
                silent: true,
              },
              concurrency: 1,
            }).then((r) => {
              setSweepProgress((p) => (p ? { ...p, done: p.done + 1 } : p));
              return r;
            })
          );

          const responses = await Promise.all(jobs);
          wallClockMs += performance.now() - startedAt;
          responses.forEach((r) => results.push(...r));
        }

        // 被中断的档位样本不完整（墙钟时间包含了取消后的空等），不纳入结果
        if (sweepStopRef.current) break;

        collected.push({
          concurrency: level,
          rounds,
          summary: summarizeRun(results, wallClockMs),
        });
        // 每完成一档立即刷新图表，长扫描也能看到趋势
        setSweepPoints([...collected]);
      }
    } catch (e) {
      setSweepError(t("sweep.failed", String(e)));
    } finally {
      setSweepRunning(false);
      setSweepProgress(null);
    }
  };

  /** 停止扫描：先置停止标记，再取消所有在途请求 */
  const stopSweep = async () => {
    sweepStopRef.current = true;
    await cancelAll();
  };

  /** 导出梯度扫描汇总（每档一行，便于后续画图或回归对比） */
  const exportSweepCsv = async () => {
    if (sweepPoints.length === 0) return;
    try {
      const path = await invoke<string>("save_text_file", {
        filename: buildExportFilename("llmperf-sweep"),
        content: "\uFEFF" + buildSweepCsv(sweepPoints),
      });
      setSweepExportHint(t("sweep.exported", path));
    } catch (e) {
      setSweepExportHint(t("sweep.exportFailed", String(e)));
    }
  };

  /** 当前是否有窗口正在发送，用于控制"全部取消"按钮的可用性 */
  const anySending = windows.some((w) => w.status === "sending");

  /**
   * 菜单命令 → 界面动作的映射表
   *
   * 用 ref 持有最新一版回调：监听只注册一次，但每次渲染都要用到
   * 当前的 mode / lastRun 等状态，否则会执行到过期的闭包。
   */
  const menuActionsRef = useRef<Record<string, () => void>>({});
  menuActionsRef.current = {
    // 发送与扫描控制按模式隔离：后端已按模式置灰，这里再兜一层，
    // 避免状态同步有延迟时在错误的模式里触发动作
    "run-send": () => {
      if (mode === "chat") handleSend();
    },
    "run-cancel-all": cancelAll,
    // 清空按当前模式区分：对话模式清窗口与汇总，扫描模式清扫描结果
    "run-clear": () => (mode === "chat" ? clearAllWindows() : clearSweepResults()),
    "run-mode-chat": () => setMode("chat"),
    "run-mode-sweep": () => setMode("sweep"),
    "run-sweep-start": () => {
      if (mode === "sweep") runSweep();
    },
    "run-sweep-stop": stopSweep,
    "file-export-run": exportCsv,
    "file-export-sweep": exportSweepCsv,
    "help-shortcuts": () => setShowShortcuts(true),
  };

  // 监听原生菜单转发的命令事件（后端只转发 ID，具体动作由这里执行）
  useEffect(() => {
    const unlisten = listen<string>("menu-command", (event) => {
      // 未映射的 ID 直接忽略，便于后续版本新增菜单项而不必改动开关
      menuActionsRef.current[event.payload]?.();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  /**
   * 把界面状态同步给原生菜单
   *
   * 依赖项只取布尔值，避免每个流式分片都触发一次 invoke。
   */
  useEffect(() => {
    invoke<void>("set_menu_state", {
      state: {
        mode,
        sending: anySending,
        sweep_running: sweepRunning,
        has_run: lastRun !== null,
        has_sweep: sweepPoints.length > 0,
      },
    }).catch((e) => console.warn("[llmperf] 同步菜单状态失败:", e));
  }, [mode, anySending, sweepRunning, lastRun, sweepPoints]);

  return (
    <div className="app-container">
      {/* 使用 currentLang 触发语言切换时的重新渲染 */}
      <div style={{ display: "none" }} aria-hidden="true" key={currentLang}></div>

      {/* ====== 顶部配置栏 ====== */}
      <div className="config-panel">
        <div className="config-header">
          <h1 className="app-title">{t("app.title")}</h1>
          {/* 模式切换：扫描进行中不允许切换，避免两个模式的请求状态互相干扰 */}
          <div className="mode-switch">
            <button
              className={`mode-btn ${mode === "chat" ? "active" : ""}`}
              onClick={() => setMode("chat")}
              disabled={sweepRunning}
            >
              {t("mode.chat")}
            </button>
            <button
              className={`mode-btn ${mode === "sweep" ? "active" : ""}`}
              onClick={() => setMode("sweep")}
              disabled={anySending}
            >
              {t("mode.sweep")}
            </button>
          </div>
        </div>

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

        {/* 采样参数：留空则使用服务端默认值。
            压测时固定 max tokens，才能让不同轮次的 tok/s 具有可比性 */}
        <div className="config-row">
          <div className="config-field">
            <label>{t("config.maxTokens")}</label>
            <input
              type="number"
              min={1}
              placeholder={t("config.maxTokensPlaceholder")}
              value={maxTokens}
              onChange={(e) => setMaxTokens(e.target.value)}
            />
          </div>

          <div className="config-field">
            <label>{t("config.temperature")}</label>
            <input
              type="number"
              min={0}
              max={2}
              step={0.1}
              placeholder={t("config.temperaturePlaceholder")}
              value={temperature}
              onChange={(e) => setTemperature(e.target.value)}
            />
          </div>
        </div>

        {/* 第三行随模式切换：对话模式配置并发数，扫描模式配置并发梯度 */}
        {mode === "chat" ? (
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

            <MessageInput value={message} onChange={setMessage} onSubmit={handleSend} />

            <div className="config-actions">
              <button
                className="btn btn-primary"
                onClick={handleSend}
                // 发送中/扫描中禁用：避免重复提交造成历史错乱
                disabled={anySending || sweepRunning}
              >
                {t("config.send")}
              </button>
              {/* 全部取消：仅在有窗口处于 sending 时才可点击，避免无意义操作 */}
              <button
                className="btn btn-danger"
                onClick={cancelAll}
                disabled={!anySending}
              >
                {t("config.cancelAll")}
              </button>
              <button className="btn btn-secondary" onClick={clearAllWindows}>
                {t("config.clear")}
              </button>
            </div>
          </div>
        ) : (
          <div className="config-row">
            <div className="config-field">
              <label>{t("sweep.levels")}</label>
              <input
                type="text"
                placeholder={t("sweep.levelsPlaceholder")}
                value={sweepLevels}
                onChange={(e) => setSweepLevels(e.target.value)}
                disabled={sweepRunning}
              />
            </div>

            <div className="config-field">
              <label>{t("sweep.rounds")}</label>
              <input
                type="number"
                min={1}
                max={5}
                value={sweepRounds}
                // 每档重复多轮可以平滑单次噪声，但轮次太多会把扫描时长拉得很长
                onChange={(e) => {
                  const n = Number(e.target.value);
                  if (!Number.isFinite(n)) return;
                  setSweepRounds(Math.min(5, Math.max(1, Math.floor(n))));
                }}
                disabled={sweepRunning}
              />
            </div>

            <MessageInput value={message} onChange={setMessage} onSubmit={runSweep} />

            <div className="config-actions">
              <button
                className="btn btn-primary"
                onClick={runSweep}
                disabled={sweepRunning || anySending}
              >
                {t("sweep.start")}
              </button>
              {/* 扫描的“停止”复用全局取消，先置停止标记避免继续跑下一档 */}
              <button
                className="btn btn-danger"
                onClick={stopSweep}
                disabled={!sweepRunning}
              >
                {t("sweep.stop")}
              </button>
              <button
                className="btn btn-secondary"
                onClick={clearSweepResults}
                disabled={sweepRunning}
              >
                {t("config.clear")}
              </button>
            </div>
          </div>
        )}
      </div>

      {/* ====== 结果区：对话模式看本轮汇总，扫描模式看梯度曲线 ====== */}
      {mode === "chat"
        ? lastRun && (
            <div className="summary-panel">
              <div className="summary-header">
                <span className="summary-title">{t("summary.title")}</span>
                <span className="summary-meta">
                  {t("summary.success", lastRun.success, lastRun.concurrency)}
                  {lastRun.canceled > 0 ? ` · ${t("summary.canceled", lastRun.canceled)}` : ""}
                  {lastRun.failed > 0 ? ` · ${t("summary.failed", lastRun.failed)}` : ""}
                  {` · ${t("summary.wallClock", (lastRun.wallClockMs / 1000).toFixed(2))}`}
                  {lastRun.throughput !== undefined
                    ? ` · ${t("summary.throughput", lastRun.throughput.toFixed(1))}`
                    : ""}
                </span>
                <button className="btn btn-secondary summary-export" onClick={exportCsv}>
                  {t("summary.export")}
                </button>
              </div>

              <table className="summary-table">
                <thead>
                  <tr>
                    <th>{t("summary.metric")}</th>
                    <th>{t("summary.avg")}</th>
                    <th>{t("summary.p50")}</th>
                    <th>{t("summary.p95")}</th>
                    <th>{t("summary.min")}</th>
                    <th>{t("summary.max")}</th>
                  </tr>
                </thead>
                <tbody>
                  <SummaryRow label={t("summary.ttft")} stats={lastRun.ttft} digits={0} />
                  {/* 正文首字：推理模型下与 TTFT 差异很大，是用户真实感知的等答案时长 */}
                  <SummaryRow
                    label={t("summary.firstContent")}
                    stats={lastRun.firstContent}
                    digits={0}
                  />
                  <SummaryRow label={t("summary.duration")} stats={lastRun.duration} digits={0} />
                  <SummaryRow
                    label={t("summary.completionTps")}
                    stats={lastRun.completionTps}
                    digits={1}
                  />
                  <SummaryRow label={t("summary.tpot")} stats={lastRun.tpot} digits={1} />
                  <SummaryRow label={t("summary.itlP95")} stats={lastRun.itlP95} digits={1} />
                </tbody>
              </table>

              {exportHint && <div className="summary-hint">{exportHint}</div>}
            </div>
          )
        : (
          <SweepPanel
            points={sweepPoints}
            running={sweepRunning}
            progress={sweepProgress}
            error={sweepError}
            exportHint={sweepExportHint}
            onExport={exportSweepCsv}
          />
        )}

      {/* ====== 底部聊天窗口区域（仅对话模式） ====== */}
      {/* 1-5 个用固定列数保持紧凑；6 个以上改由 CSS 按可用宽度自适应列数 */}
      {mode === "chat" && (
        <div className={`chat-grid ${concurrency <= 5 ? `cols-${concurrency}` : "cols-auto"}`}>
          {windows.map((win) => (
            <ChatWindow
              key={win.id}
              window={win}
              onCancel={() => cancelWindow(win.id)}
            />
          ))}
        </div>
      )}

      {/* 关于对话框：用应用内模态框，避免 Tauri 在 alert 上注入 localhost URL */}
      {showAbout && (
        <div
          className="modal-backdrop"
          onClick={() => setShowAbout(false)}
        >
          <div
            className="modal-dialog"
            role="dialog"
            aria-modal="true"
            onClick={(e) => e.stopPropagation()}
          >
            <h2 className="modal-title">{t("app.title")}</h2>
            <div className="modal-body">
              {t("about.versionLabel")}: {t("about.version", "0.1.0")}
            </div>
            <div className="modal-actions">
              <button
                className="btn btn-primary"
                onClick={() => setShowAbout(false)}
                autoFocus
              >
                {t("about.ok")}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 快捷键一览：数据来自后端菜单定义，切换语言后文案随 t() 自动更新 */}
      {showShortcuts && (
        <div className="modal-backdrop" onClick={() => setShowShortcuts(false)}>
          <div
            className="modal-dialog"
            role="dialog"
            aria-modal="true"
            onClick={(e) => e.stopPropagation()}
          >
            <h2 className="modal-title">{t("shortcuts.title")}</h2>
            <div className="modal-body shortcut-body">
              <p className="shortcut-hint">{t("shortcuts.hint")}</p>
              <table className="shortcut-table">
                <tbody>
                  {shortcutRows.map((row) => (
                    <tr key={row.id}>
                      <td>{t(`menu.${row.key}`)}</td>
                      <td className="shortcut-accel">
                        {formatAccel(row.accel).map((key, i) => (
                          <kbd className="kbd" key={`${row.id}-${i}`}>
                            {key}
                          </kbd>
                        ))}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <div className="modal-actions">
              <button
                className="btn btn-primary"
                onClick={() => setShowShortcuts(false)}
                autoFocus
              >
                {t("shortcuts.close")}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/* ====== 消息输入框（对话模式与扫描模式共用） ====== */
function MessageInput({
  value,
  onChange,
  onSubmit,
}: {
  value: string;
  onChange: (value: string) => void;
  onSubmit: () => void;
}) {
  return (
    <div className="config-field flex-1">
      <label>{t("config.sendMessage")}</label>
      <input
        type="text"
        placeholder={t("config.messagePlaceholder")}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") onSubmit();
        }}
      />
    </div>
  );
}

/* ====== 汇总表格中的一行指标 ====== */
function SummaryRow({
  label,
  stats,
  digits,
}: {
  label: string;
  stats: Stats;
  digits: number;
}) {
  const fmt = (value?: number) => (value === undefined ? "—" : value.toFixed(digits));
  return (
    <tr>
      <td>{label}</td>
      <td>{fmt(stats.avg)}</td>
      <td>{fmt(stats.p50)}</td>
      <td>{fmt(stats.p95)}</td>
      <td>{fmt(stats.min)}</td>
      <td>{fmt(stats.max)}</td>
    </tr>
  );
}

/* ====== 单个聊天窗口的关键指标 ====== */
function ChatMetric({
  label,
  value,
  unit,
  digits = 0,
}: {
  label: string;
  value?: number;
  unit: string;
  digits?: number;
}) {
  // 无数据（取消、或服务端未返回 usage）时不渲染，避免一排「—」占位
  if (value === undefined || !Number.isFinite(value) || value <= 0) return null;
  return (
    <span className="chat-metric">
      <span className="chat-metric-label">{label}</span>
      <span className="chat-metric-value">
        {value.toFixed(digits)}
        {unit}
      </span>
    </span>
  );
}

/* ====== 单个聊天窗口组件 ====== */
function ChatWindow({
  window: win,
  onCancel,
}: {
  window: ChatWindow;
  /** 用户点击"取消"按钮时触发，参数语义对应后端 window_id */
  onCancel: () => void;
}) {
  const statusLabel = () => {
    const contentLen = win.accumulatedContent.length;
    const reasoningLen = win.accumulatedReasoning.length;
    // 格式化工具：保留两位小数，去掉无意义的末尾零
    const fmtTps = (tps?: number, label?: string) =>
      tps !== undefined && tps > 0 ? ` ${label} ${tps.toFixed(2)} tok/s` : "";
    // 首字延迟：无有效值时返回空串，避免模板里出现空的括号内容
    const fmtTtft = (ms?: number) =>
      ms !== undefined && ms > 0 ? t("status.ttft", Math.round(ms)) : "";
    switch (win.status) {
      case "idle":
        return t("status.idle");
      case "sending":
        if (reasoningLen > 0) {
          return t("status.sending_with_reasoning", contentLen, reasoningLen);
        }
        return t("status.sending", contentLen);
      case "done":
        return t("status.done", win.duration ?? 0, fmtTps(win.completionTps, t("status.completion_label")), fmtTps(win.promptTps, t("status.prompt_label")), fmtTtft(win.ttft));
      case "error":
        return t("status.error", win.duration ?? 0, fmtTps(win.completionTps, t("status.completion_label")), fmtTps(win.promptTps, t("status.prompt_label")), fmtTtft(win.ttft));
      case "canceled":
        return t("status.canceled", win.duration ?? 0);
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
        <div className="chat-header-right">
          <span className="chat-status">{statusLabel()}</span>
          {/* 仅在 sending 时显示单窗口"取消"按钮，避免误触 */}
          {win.status === "sending" && (
            <button
              className="btn-cancel-window"
              onClick={onCancel}
              title={t("config.cancel")}
            >
              {t("config.cancel")}
            </button>
          )}
        </div>
      </div>

      {/* 完成后的关键指标：每窗口独立展示，方便横向对比抖动与 TPOT */}
      {(win.status === "done" || win.status === "error") && (
        <div className="chat-metrics">
          <ChatMetric label={t("metrics.ttft")} value={win.ttft} unit="ms" />
          <ChatMetric label={t("metrics.content")} value={win.firstContent} unit="ms" />
          <ChatMetric label="TPOT" value={win.tpot} unit="ms" digits={1} />
          <ChatMetric label="ITL95" value={win.itlP95} unit="ms" digits={0} />
          <ChatMetric label={t("metrics.tps")} value={win.completionTps} unit="" digits={1} />
        </div>
      )}

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
                  {/* 耗时为 0 / 缺失时 formatDuration 返回空串，此时不渲染分隔符，避免出现孤立的「·」 */}
                  {formatDuration(msg.reasoningDuration) && (
                    <span className="reasoning-duration">
                      {" · "}
                      {formatDuration(msg.reasoningDuration)}
                    </span>
                  )}
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
        {/* 取消提示：保留已流出的部分内容，再用浅色条说明这是被取消的输出 */}
        {win.status === "canceled" && (
          <div className="chat-canceled">
            <strong>{t("chat.canceled")}</strong>
          </div>
        )}
      </div>
    </div>
  );
}

export default App;
