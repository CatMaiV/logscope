/**
 * 展示格式化工具。
 * 时间统一按工作区时区（默认 +480）显示墙钟时间。
 */

export const DEFAULT_TZ_MIN = 480;

/** UTC 毫秒 → 工作区时区的 "YYYY-MM-DD HH:MM:SS.mmm" */
export function fmtMs(ms: number | null | undefined, tz = DEFAULT_TZ_MIN): string {
  if (ms === null || ms === undefined) return "??:??:??.???";
  const d = new Date(ms + tz * 60_000);
  return d.toISOString().slice(0, 23).replace("T", " ");
}

/** 仅时分秒毫秒（列表行内紧凑显示） */
export function fmtMsShort(ms: number | null | undefined, tz = DEFAULT_TZ_MIN): string {
  if (ms === null || ms === undefined) return "??:??:??.???";
  const d = new Date(ms + tz * 60_000);
  return d.toISOString().slice(11, 23);
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

export function fmtCount(n: number): string {
  if (n < 10_000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(2)}M`;
}

/** 级别 → 颜色（与 ECharts 时间轴/列表共用；不只靠颜色，行内始终有级别文本） */
export const LEVEL_COLORS: Record<string, string> = {
  TRACE: "#7c8699",
  DEBUG: "#7c8699",
  INFO: "#4f9cf9",
  WARN: "#e0a84e",
  ERROR: "#e05c5c",
  FATAL: "#c34ad0",
  UNKNOWN: "#8a8f98",
};

/** 复制文本到剪贴板（WebView2 环境；失败时回退 execCommand） */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      document.body.removeChild(ta);
      return ok;
    } catch {
      return false;
    }
  }
}
