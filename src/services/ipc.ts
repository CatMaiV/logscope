/**
 * Tauri invoke / Channel 封装。
 * M0 仅占位；命令实现随 M1 逐步落地，类型与 src-tauri DTO 保持同步。
 */
import { invoke } from "@tauri-apps/api/core";

/** 稳定事件标识（与 logscope-core `EventRef` 对应） */
export interface EventRef {
  file_id: number;
  sequence_no: number;
}

export interface FileSummary {
  file_id: number;
  file_name: string;
  path: string;
  file_size: number;
  encoding: string;
  profile: string | null;
  state: string;
  events: number;
  warn_count: number;
  error_count: number;
  time_min_ms: number | null;
  time_max_ms: number | null;
}

/** 列出当前已导入文件（M1 接通） */
export async function listFiles(): Promise<FileSummary[]> {
  return invoke<FileSummary[]>("list_files");
}
