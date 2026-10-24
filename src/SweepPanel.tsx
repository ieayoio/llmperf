/**
 * 并发梯度扫描结果面板
 *
 * 展示每一档并发的汇总指标、吞吐/延迟曲线，并支持导出 CSV。
 * 图表用手写 SVG 实现，避免为几根折线引入图表库。
 */

import { t } from "./i18n";
import type { RunSummary, SweepPoint } from "./metrics";

/** 扫描进行中的进度（当前档位已完成多少路请求） */
export interface SweepProgress {
  concurrency: number;
  done: number;
  total: number;
  /** 当前轮次（从 1 开始） */
  round: number;
  rounds: number;
}

export interface SweepPanelProps {
  points: SweepPoint[];
  running: boolean;
  progress: SweepProgress | null;
  error: string;
  exportHint: string;
  onExport: () => void;
}

/** 表格单元格格式化：无数据显示占位符，避免出现 NaN */
function fmt(value: number | undefined, digits = 0): string {
  return value === undefined || !Number.isFinite(value) ? "—" : value.toFixed(digits);
}

export function SweepPanel({
  points,
  running,
  progress,
  error,
  exportHint,
  onExport,
}: SweepPanelProps) {
  return (
    <div className="summary-panel sweep-panel">
      <div className="summary-header">
        <span className="summary-title">{t("sweep.title")}</span>
        <span className="summary-meta">{t("sweep.meta", points.length)}</span>
        <button
          className="btn btn-secondary summary-export"
          onClick={onExport}
          disabled={points.length === 0}
        >
          {t("sweep.export")}
        </button>
      </div>

      {running && progress && (
        <div className="sweep-progress">
          <div className="sweep-progress-text">
            {t("sweep.progress", progress.concurrency, progress.done, progress.total)}
            {progress.rounds > 1
              ? t("sweep.progressRound", progress.round, progress.rounds)
              : ""}
          </div>
          <div className="sweep-progress-bar">
            <div
              className="sweep-progress-fill"
              style={{
                width: `${Math.round((progress.done / Math.max(1, progress.total)) * 100)}%`,
              }}
            />
          </div>
        </div>
      )}

      {error && <div className="summary-hint sweep-error">{error}</div>}

      {points.length === 0
        ? !running && <div className="summary-hint">{t("sweep.empty")}</div>
        : (
          <>
            <div className="sweep-charts">
              <SweepChart
                title={t("sweep.chartThroughput")}
                unit="tok/s"
                points={points}
                pick={(s) => s.throughput}
                color="#a78bfa"
                digits={1}
              />
              <SweepChart
                title={t("sweep.chartTtft")}
                unit="ms"
                points={points}
                pick={(s) => s.ttft.p95}
                color="#38bdf8"
                digits={0}
              />
              <SweepChart
                title={t("sweep.chartTpot")}
                unit="ms"
                points={points}
                pick={(s) => s.tpot.avg}
                color="#fbbf24"
                digits={1}
              />
            </div>

            <table className="summary-table sweep-table">
              <thead>
                <tr>
                  <th>{t("sweep.concurrency")}</th>
                  <th>{t("sweep.success")}</th>
                  <th>{t("sweep.throughput")}</th>
                  <th>{t("sweep.ttftP50")}</th>
                  <th>{t("sweep.ttftP95")}</th>
                  <th>{t("sweep.firstContent")}</th>
                  <th>{t("sweep.tpot")}</th>
                  <th>{t("sweep.itlP95")}</th>
                  <th>{t("sweep.durationP95")}</th>
                </tr>
              </thead>
              <tbody>
                {points.map((p) => (
                  <tr key={p.concurrency}>
                    <td>{p.concurrency}</td>
                    <td>
                      {p.summary.success}/{p.summary.failed}
                      {p.summary.canceled > 0 ? ` / ${p.summary.canceled}` : ""}
                    </td>
                    <td>{fmt(p.summary.throughput, 1)}</td>
                    <td>{fmt(p.summary.ttft.p50)}</td>
                    <td>{fmt(p.summary.ttft.p95)}</td>
                    <td>{fmt(p.summary.firstContent.p50)}</td>
                    <td>{fmt(p.summary.tpot.avg, 1)}</td>
                    <td>{fmt(p.summary.itlP95.avg, 1)}</td>
                    <td>{fmt(p.summary.duration.p95)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </>
        )}

      {exportHint && <div className="summary-hint">{exportHint}</div>}
    </div>
  );
}

/** 图表尺寸与内边距（viewBox 坐标系，随容器等比缩放） */
const CHART = { width: 340, height: 156, left: 48, right: 12, top: 14, bottom: 26 };

/** 单指标折线图：横轴并发档位，纵轴指标数值 */
function SweepChart({
  title,
  unit,
  points,
  pick,
  color,
  digits,
}: {
  title: string;
  unit: string;
  points: SweepPoint[];
  /** 从一轮汇总中取出要画的值，缺失的档位会被跳过 */
  pick: (summary: RunSummary) => number | undefined;
  color: string;
  digits: number;
}) {
  const data = points
    .map((p) => ({ x: p.concurrency, y: pick(p.summary) }))
    .filter((d): d is { x: number; y: number } =>
      typeof d.y === "number" && Number.isFinite(d.y)
    );

  const innerW = CHART.width - CHART.left - CHART.right;
  const innerH = CHART.height - CHART.top - CHART.bottom;

  const xMin = data.length > 0 ? Math.min(...data.map((d) => d.x)) : 0;
  const xMax = data.length > 0 ? Math.max(...data.map((d) => d.x)) : 1;
  // 纵轴从 0 起：吞吐与延迟都适合以 0 为基准，避免视觉上夸大差异
  const yTop = data.length > 0 ? Math.max(...data.map((d) => d.y)) * 1.15 || 1 : 1;

  const px = (x: number) =>
    xMax === xMin
      ? CHART.left + innerW / 2
      : CHART.left + ((x - xMin) / (xMax - xMin)) * innerW;
  const py = (y: number) => CHART.top + innerH - (y / yTop) * innerH;

  const path = data.map((d, i) => `${i === 0 ? "M" : "L"}${px(d.x)},${py(d.y)}`).join(" ");

  return (
    <div className="sweep-chart">
      <div className="sweep-chart-head">
        <span>{title}</span>
        <span className="sweep-chart-unit">{unit}</span>
      </div>

      {data.length === 0 ? (
        <div className="sweep-chart-empty">{t("sweep.noData")}</div>
      ) : (
        <svg
          className="sweep-chart-svg"
          viewBox={`0 0 ${CHART.width} ${CHART.height}`}
          role="img"
          aria-label={title}
        >
          {/* 横向网格线与纵轴刻度 */}
          {[0, 0.5, 1].map((ratio) => {
            const y = CHART.top + innerH * (1 - ratio);
            return (
              <g key={ratio}>
                <line
                  x1={CHART.left}
                  y1={y}
                  x2={CHART.width - CHART.right}
                  y2={y}
                  stroke="#2a2a35"
                  strokeWidth={1}
                />
                <text
                  x={CHART.left - 6}
                  y={y + 3}
                  textAnchor="end"
                  fontSize={9}
                  fill="#6b7280"
                >
                  {(yTop * ratio).toFixed(digits)}
                </text>
              </g>
            );
          })}

          {/* 横轴：并发档位 */}
          {data.map((d) => (
            <text
              key={`x-${d.x}`}
              x={px(d.x)}
              y={CHART.height - CHART.bottom + 14}
              textAnchor="middle"
              fontSize={9}
              fill="#6b7280"
            >
              {d.x}
            </text>
          ))}

          <path d={path} fill="none" stroke={color} strokeWidth={2} strokeLinejoin="round" />

          {data.map((d) => (
            <circle key={`p-${d.x}`} cx={px(d.x)} cy={py(d.y)} r={3} fill={color}>
              <title>{`${d.x} → ${d.y.toFixed(digits)} ${unit}`}</title>
            </circle>
          ))}
        </svg>
      )}
    </div>
  );
}
