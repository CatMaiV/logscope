//! 文件导入命令：open_files / get_task_status / cancel_import。

use super::{CmdError, CmdResult};
use crate::state::{AppState, TaskKind, TaskStatus};
use logscope_core::detail::DetailReader;
use logscope_core::index::{build_workspace, FileIndex};
use logscope_core::model::stats::{ScanCallbacks, ScanOptions, ScanProgress};
use logscope_core::scanner::scan_file;
use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::{Manager, State};

/// 导入设置
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportSettings {
    /// "auto" | "springboot" | "logback-std" | "logback-nothread" | "raw"
    pub profile: Option<String>,
    pub encoding: Option<String>,
    /// 分钟偏移，默认 480（Asia/Shanghai）
    pub tz_offset_min: Option<i32>,
    pub max_event_bytes: Option<usize>,
    /// 自定义解析正则（M4）：必须含 `ts` 与 `level` 命名组，优先于自动识别
    pub custom_pattern: Option<String>,
}

/// 导入进度事件（点对点 Channel，保证有序）
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    pub task_id: String,
    pub file: String,
    pub phase: String, // detect | indexing | done | cancelled | failed
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub events: u64,
    pub message: Option<String>,
}

/// 单文件导入结果（进度 Done 附带）
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSummaryDto {
    pub file_id: u32,
    pub file_name: String,
    pub path: String,
    pub file_size: u64,
    pub encoding: String,
    pub profile: Option<String>,
    pub state: String,
    pub events: usize,
    pub warn_count: u64,
    pub error_count: u64,
    pub unknown_count: u64,
    pub time_min_ms: Option<i64>,
    pub time_max_ms: Option<i64>,
    pub truncated_events: u32,
    pub duration_ms: f64,
}

/// 打开并导入一个或多个日志文件。立即返回任务 ID，进度经 `on_progress` Channel 推送。
#[tauri::command]
pub async fn open_files(
    app: tauri::AppHandle,
    paths: Vec<String>,
    settings: ImportSettings,
    on_progress: Channel<ImportProgress>,
) -> CmdResult<String> {
    if paths.is_empty() {
        return Err(CmdError::new("bad-request", "路径列表为空"));
    }
    let state: State<'_, AppState> = app.state();
    let task_id = uuid::Uuid::new_v4().to_string();
    let handle = Arc::new(crate::state::TaskHandle::new(
        task_id.clone(),
        TaskKind::Import,
    ));
    state.register_task(handle.clone());

    let tz = settings
        .tz_offset_min
        .unwrap_or(state.default_tz_offset_min);
    let opts = ScanOptions {
        profile_hint: settings
            .profile
            .as_deref()
            .and_then(logscope_core::scanner::profile_id_from_str),
        encoding_hint: settings
            .encoding
            .as_deref()
            .and_then(logscope_core::scanner::encoding_from_str),
        custom_pattern: settings
            .custom_pattern
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string),
        default_tz_offset_min: tz,
        max_event_bytes: settings.max_event_bytes.unwrap_or(8 * 1024 * 1024),
        ..Default::default()
    };

    // 后台执行：每文件一个 blocking 任务，Semaphore 限并发
    let app2 = app.clone();
    let task_id_out = task_id.clone();
    tauri::async_runtime::spawn(async move {
        let st: State<'_, AppState> = app2.state();
        handle.set_status(TaskStatus::Running);
        let mut total = 0usize;
        let mut failed = 0usize;

        for path in paths {
            if handle.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            // 并发许可（阻塞等待不占 UI 线程）
            let permit = st.scan_permits.clone().acquire_owned().await;
            let file_id = st.alloc_file_id();
            let path_display = path.clone();
            let tid = task_id.clone();
            let ch = on_progress.clone();
            let cancel_flag = handle.cancel.clone();
            let opts = opts.clone();

            let result = tauri::async_runtime::spawn_blocking(move || {
                let cb = ScanCallbacks {
                    progress: Some(Arc::new(move |p: ScanProgress| {
                        let phase = match p.phase {
                            logscope_core::model::stats::ScanPhase::Detect => "detect",
                            logscope_core::model::stats::ScanPhase::Indexing => "indexing",
                            logscope_core::model::stats::ScanPhase::Done => "done",
                            logscope_core::model::stats::ScanPhase::Cancelled => "cancelled",
                            logscope_core::model::stats::ScanPhase::Failed => "failed",
                        };
                        let _ = ch.send(ImportProgress {
                            task_id: tid.clone(),
                            file: p.path.display().to_string(),
                            phase: phase.to_string(),
                            bytes_done: p.bytes_done,
                            bytes_total: p.bytes_total,
                            events: p.events,
                            message: None,
                        });
                    })),
                    cancel: Some(cancel_flag),
                };
                scan_file(std::path::Path::new(&path), &opts, &cb, file_id)
            })
            .await;
            total += 1;

            match result {
                Ok(Ok(out)) => {
                    // 注册：短暂写锁，不跨 await 持有
                    let summary = {
                        let mut ws = st.workspace.write();
                        let idx = Arc::new(FileIndex::new(
                            out.meta.clone(),
                            out.entries,
                            out.stats.clone(),
                        ));
                        let summary = file_summary(&out.meta, &out.stats);
                        match DetailReader::open(&out.meta, tz) {
                            Ok(r) => {
                                ws.readers
                                    .insert(file_id, Arc::new(parking_lot::Mutex::new(r)));
                            }
                            Err(e) => {
                                // 刚扫完就打不开读取器（文件被并发改动等极端情况）：
                                // 索引保留但无读取器，详情读取时返回 not-found
                                eprintln!("DetailReader open failed: {}", e);
                            }
                        }
                        // 重建全局索引（M1 文件数少，整体重建开销可接受）
                        let mut files: Vec<Arc<FileIndex>> =
                            ws.index.files.values().cloned().collect();
                        let idx2 = idx.clone();
                        files.push(idx2);
                        ws.index = build_workspace(files);
                        summary
                    };
                    let _ = on_progress.send(ImportProgress {
                        task_id: task_id.clone(),
                        file: path_display.clone(),
                        phase: "file-done".into(),
                        bytes_done: 0,
                        bytes_total: 0,
                        events: summary.events as u64,
                        message: serde_json::to_string(&summary).ok(),
                    });
                }
                Ok(Err(logscope_core::CoreError::Cancelled)) => {
                    let _ = on_progress.send(ImportProgress {
                        task_id: task_id.clone(),
                        file: path_display,
                        phase: "cancelled".into(),
                        bytes_done: 0,
                        bytes_total: 0,
                        events: 0,
                        message: None,
                    });
                    handle.set_status(TaskStatus::Cancelled);
                    drop(permit);
                    return; // 取消：不注册半成品
                }
                Ok(Err(e)) => {
                    let _ = on_progress.send(ImportProgress {
                        task_id: task_id.clone(),
                        file: path_display,
                        phase: "failed".into(),
                        bytes_done: 0,
                        bytes_total: 0,
                        events: 0,
                        message: Some(e.to_string()),
                    });
                    failed += 1;
                    // 单文件失败不中断其余文件
                }
                Err(join_err) => {
                    let _ = on_progress.send(ImportProgress {
                        task_id: task_id.clone(),
                        file: path_display,
                        phase: "failed".into(),
                        bytes_done: 0,
                        bytes_total: 0,
                        events: 0,
                        message: Some(format!("任务异常: {}", join_err)),
                    });
                    failed += 1;
                }
            }
            drop(permit);
        }

        let status = handle.status();
        if matches!(status, TaskStatus::Cancelled) {
            // 保持 Cancelled
        } else if total > 0 && failed == total {
            handle.set_status(TaskStatus::Failed(format!("全部 {} 个文件导入失败", total)));
        } else {
            handle.set_status(TaskStatus::Done);
        }
        // 整批终态事件（前端等待导入完成的信号）
        let final_phase = match handle.status() {
            TaskStatus::Done => "all-done",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Failed(ref m) => {
                let _ = m;
                "failed"
            }
            _ => "all-done",
        };
        let final_msg = match handle.status() {
            TaskStatus::Failed(ref m) => Some(m.clone()),
            _ => None,
        };
        let _ = on_progress.send(ImportProgress {
            task_id: task_id.clone(),
            file: format!("{} 个文件（{} 失败）", total, failed),
            phase: final_phase.to_string(),
            bytes_done: 0,
            bytes_total: 0,
            events: 0,
            message: final_msg,
        });
        st.prune_tasks();
    });

    Ok(task_id_out)
}

/// 查询任务状态（Channel 消息丢失后的兜底）。
#[tauri::command]
pub fn get_task_status(
    task_id: String,
    state: State<'_, AppState>,
) -> CmdResult<serde_json::Value> {
    let h = state
        .get_task(&task_id)
        .ok_or_else(|| CmdError::new("not-found", format!("任务不存在: {}", task_id)))?;
    let status = match h.status() {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Done => "done",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Failed(_) => "failed",
    };
    let message = match h.status() {
        TaskStatus::Failed(m) => Some(m),
        _ => None,
    };
    Ok(serde_json::json!({
        "taskId": task_id,
        "kind": match h.kind { crate::state::TaskKind::Import => "import", crate::state::TaskKind::Search => "search" },
        "status": status,
        "message": message,
    }))
}

/// 取消导入任务。
#[tauri::command]
pub fn cancel_import(task_id: String, state: State<'_, AppState>) -> CmdResult<bool> {
    let h = state
        .get_task(&task_id)
        .ok_or_else(|| CmdError::new("not-found", format!("任务不存在: {}", task_id)))?;
    Ok(h.cancel())
}

/// 由元数据+统计生成文件摘要 DTO
pub fn file_summary(
    meta: &logscope_core::model::file_meta::FileMeta,
    stats: &logscope_core::model::stats::ParseStats,
) -> FileSummaryDto {
    FileSummaryDto {
        file_id: meta.file_id,
        file_name: meta.file_name().to_string(),
        path: meta.path.display().to_string(),
        file_size: meta.file_size,
        encoding: meta.encoding.as_str().to_string(),
        profile: meta.profile.map(|p| p.display_name()),
        state: "ready".into(),
        events: stats.events,
        warn_count: stats.warn_count(),
        error_count: stats.error_count(),
        unknown_count: stats.unknown_count(),
        time_min_ms: stats.time_min_ms,
        time_max_ms: stats.time_max_ms,
        truncated_events: stats.truncated_events,
        duration_ms: stats.duration.as_secs_f64() * 1000.0,
    }
}

/// 解析预览行
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRowDto {
    pub line: String,
    pub matched: bool,
    pub timestamp_ms: Option<i64>,
    pub level: Option<String>,
    pub thread: Option<String>,
    pub logger: Option<String>,
}

/// 解析预览结果
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDto {
    /// 未提供自定义规则时，自动识别到的内置规则名
    pub detected: Option<String>,
    pub matched: usize,
    pub sampled: usize,
    pub rows: Vec<PreviewRowDto>,
}

/// 解析预览（M4）：不导入、不建索引，只采样文件头部若干行并报告识别情况。
/// `pattern` 为空时使用自动识别；否则校验并试用自定义正则。
#[tauri::command]
pub async fn preview_parse(
    file: String,
    pattern: Option<String>,
    tz_offset_min: Option<i32>,
    max_lines: Option<usize>,
    state: State<'_, AppState>,
) -> CmdResult<PreviewDto> {
    let tz = tz_offset_min.unwrap_or(state.default_tz_offset_min);
    let max_lines = max_lines.unwrap_or(40);
    let path = std::path::PathBuf::from(file);
    let pat = pattern
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    let preview = tauri::async_runtime::spawn_blocking(move || {
        logscope_core::parser::preview::preview_file(&path, pat.as_deref(), tz, max_lines)
    })
    .await
    .map_err(|e| CmdError::new("join", e.to_string()))??;
    Ok(PreviewDto {
        detected: preview.detected,
        matched: preview.matched,
        sampled: preview.sampled,
        rows: preview
            .rows
            .into_iter()
            .map(|r| PreviewRowDto {
                line: r.line,
                matched: r.matched,
                timestamp_ms: r.timestamp_ms,
                level: r.level,
                thread: r.thread,
                logger: r.logger,
            })
            .collect(),
    })
}
