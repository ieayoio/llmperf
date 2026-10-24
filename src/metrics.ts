/**
 * 性能指标聚合工具
 *
 * 把后端返回的逐窗口结果（SingleResult）聚合成可展示的统计量，并生成 CSV。
 * 统计口径与后端 `request.rs` 保持一致：只统计成功样本，分位数用线性插值。
 */

/** 流式增量间隔（ITL）统计 */
export interface ItlStats {
  /** 分片间隔中位数（毫秒） */
  p50_ms: number;
  /** 分片间隔 P95（毫秒） */
  p95_ms: number;
  /** 分片间隔最大值（毫秒） */
  max_ms: number;
}

/** 后端 `SingleResult`（Rust 字段名原样序列化，故为 snake_case） */
export interface SingleResult {
  window_id: number;
  duration_ms: number;
  /** 首 token（思考或正文）延迟，收到首个增量前失败时缺失 */
  ttft_ms?: number;
  /** 正文首字延迟：推理模型会先输出思考内容，这个值才是用户看到答案的时间 */
  first_content_ms?: number;
  /** 平均分片间隔，近似 TPOT（Time Per Output Token） */
  tpot_ms?: number;
  /** 分片间隔分位数统计 */
  itl?: ItlStats;
  prompt_tokens?: number;
  completion_tokens?: number;
  completion_tokens_per_second?: number;
  prompt_tokens_per_second?: number;
  /** 是否被用户主动取消 */
  canceled?: boolean;
  error: string | null;
}

/** 一组数值的统计量（无有效数据时各字段缺失） */
export interface Stats {
  avg?: number;
  min?: number;
  max?: number;
  p50?: number;
  p95?: number;
}

/** 一轮并发测试的汇总结果 */
export interface RunSummary {
  concurrency: number;
  /** 从发起请求到全部窗口结束的墙钟时间（毫秒） */
  wallClockMs: number;
  success: number;
  failed: number;
  canceled: number;
  totalCompletionTokens: number;
  /** 整体吞吐：本轮总 completion token / 墙钟秒数 */
  throughput?: number;
  ttft: Stats;
  firstContent: Stats;
  duration: Stats;
  completionTps: Stats;
  tpot: Stats;
  /** 各窗口 ITL P95 的分布（单窗口时即为该窗口的值） */
  itlP95: Stats;
  /** 逐窗口明细，供导出 CSV 使用 */
  results: SingleResult[];
}

/** 梯度扫描中一个并发档位的结果 */
export interface SweepPoint {
  /** 该档位的并发数 */
  concurrency: number;
  /** 该档位重复的轮次 */
  rounds: number;
  summary: RunSummary;
}

/** 线性插值百分位：与 numpy.percentile 默认口径一致 */
function percentileOfSorted(sorted: number[], p: number): number {
  if (sorted.length === 1) return sorted[0];
  const idx = (sorted.length - 1) * p;
  const lo = Math.floor(idx);
  const hi = Math.ceil(idx);
  return sorted[lo] + (sorted[hi] - sorted[lo]) * (idx - lo);
}

/** 计算一组数值的均值、最小/最大与 P50/P95；空数组返回空对象 */
export function computeStats(values: number[]): Stats {
  if (values.length === 0) return {};
  const sorted = [...values].sort((a, b) => a - b);
  return {
    avg: sorted.reduce((sum, v) => sum + v, 0) / sorted.length,
    min: sorted[0],
    max: sorted[sorted.length - 1],
    p50: percentileOfSorted(sorted, 0.5),
    p95: percentileOfSorted(sorted, 0.95),
  };
}

/** 把逐窗口结果聚合成一轮的汇总统计 */
export function summarizeRun(results: SingleResult[], wallClockMs: number): RunSummary {
  const canceled = results.filter((r) => r.canceled === true).length;
  const failed = results.filter((r) => r.canceled !== true && r.error).length;
  // 只统计成功且有数值的样本，避免失败/取消把均值带偏
  const succeeded = results.filter((r) => !r.error && r.canceled !== true);
  const samples = (pick: (r: SingleResult) => number | undefined) =>
    succeeded
      .map(pick)
      .filter((v): v is number => typeof v === "number" && v > 0);

  const totalCompletionTokens = results.reduce(
    (sum, r) => sum + (r.completion_tokens ?? 0),
    0
  );

  return {
    concurrency: results.length,
    wallClockMs,
    success: results.length - failed - canceled,
    failed,
    canceled,
    totalCompletionTokens,
    throughput:
      wallClockMs > 0 && totalCompletionTokens > 0
        ? totalCompletionTokens / (wallClockMs / 1000)
        : undefined,
    ttft: computeStats(samples((r) => r.ttft_ms)),
    firstContent: computeStats(samples((r) => r.first_content_ms)),
    duration: computeStats(samples((r) => r.duration_ms)),
    completionTps: computeStats(samples((r) => r.completion_tokens_per_second)),
    tpot: computeStats(samples((r) => r.tpot_ms)),
    itlP95: computeStats(samples((r) => r.itl?.p95_ms)),
    results,
  };
}

/** CSV 字段转义：含逗号、引号或换行时用双引号包裹 */
function csvCell(value: string | number | undefined | null): string {
  if (value === undefined || value === null) return "";
  const text = String(value);
  return /[",\n\r]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

/** 把一行单元格拼成 CSV 行 */
function csvRow(cells: (string | number | undefined | null)[]): string {
  return cells.map(csvCell).join(",");
}

/** 生成逐窗口明细 CSV（表头固定英文，便于在 Excel / 脚本中复用） */
export function buildCsv(results: SingleResult[]): string {
  const header = csvRow([
    "window",
    "status",
    "ttft_ms",
    "first_content_ms",
    "duration_ms",
    "tpot_ms",
    "itl_p50_ms",
    "itl_p95_ms",
    "itl_max_ms",
    "prompt_tokens",
    "completion_tokens",
    "completion_tps",
    "prompt_tps",
    "error",
  ]);

  const rows = results.map((r) =>
    csvRow([
      r.window_id + 1,
      // 取消优先判定，避免依赖 error 文案是否保留
      r.canceled ? "canceled" : r.error ? "error" : "ok",
      r.ttft_ms ?? "",
      r.first_content_ms ?? "",
      r.duration_ms,
      r.tpot_ms?.toFixed(2) ?? "",
      r.itl?.p50_ms.toFixed(2) ?? "",
      r.itl?.p95_ms.toFixed(2) ?? "",
      r.itl?.max_ms.toFixed(2) ?? "",
      r.prompt_tokens ?? "",
      r.completion_tokens ?? "",
      r.completion_tokens_per_second?.toFixed(2) ?? "",
      r.prompt_tokens_per_second?.toFixed(2) ?? "",
      r.error ?? "",
    ])
  );

  return [header, ...rows].join("\n");
}

/** 生成梯度扫描汇总 CSV：每档一行，便于画图或做回归对比 */
export function buildSweepCsv(points: SweepPoint[]): string {
  const header = csvRow([
    "concurrency",
    "rounds",
    "success",
    "failed",
    "canceled",
    "wall_clock_ms",
    "throughput_tps",
    "ttft_p50_ms",
    "ttft_p95_ms",
    "first_content_p50_ms",
    "tpot_avg_ms",
    "completion_tps_p50",
    "duration_p95_ms",
    "itl_p95_avg_ms",
  ]);

  const rows = points.map((p) => {
    const s = p.summary;
    const fix = (v: number | undefined, digits = 2) =>
      v === undefined ? "" : Number(v.toFixed(digits));
    return csvRow([
      p.concurrency,
      p.rounds,
      s.success,
      s.failed,
      s.canceled,
      Math.round(s.wallClockMs),
      fix(s.throughput),
      fix(s.ttft.p50, 0),
      fix(s.ttft.p95, 0),
      fix(s.firstContent.p50, 0),
      fix(s.tpot.avg),
      fix(s.completionTps.p50),
      fix(s.duration.p95, 0),
      fix(s.itlP95.avg),
    ]);
  });

  return [header, ...rows].join("\n");
}

/**
 * 解析并发梯度输入
 *
 * 支持中英文逗号与空格分隔，自动去重、升序、裁剪到 [1, 50]，最多 8 档
 * （档位过多时单次扫描耗时会长到没人愿意等）。
 */
export function parseLevels(raw: string, limit = 8): number[] {
  const values = raw
    .split(/[,，\s]+/)
    .map((s) => Number(s.trim()))
    .filter((n) => Number.isFinite(n) && n >= 1)
    .map((n) => Math.min(50, Math.floor(n)));
  return Array.from(new Set(values))
    .sort((a, b) => a - b)
    .slice(0, limit);
}

/** 导出文件名：{prefix}-YYYYMMDD-HHmmss.csv */
export function buildExportFilename(prefix: string): string {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  const date = `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}`;
  const time = `${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`;
  return `${prefix}-${date}-${time}.csv`;
}

/** 解析可选数字输入：空串或非法值返回 null（交由后端使用服务端默认值） */
export function parseOptionalNumber(raw: string): number | null {
  const text = raw.trim();
  if (text === "") return null;
  const value = Number(text);
  return Number.isFinite(value) ? value : null;
}
