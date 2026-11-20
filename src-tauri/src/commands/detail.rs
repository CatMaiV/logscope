//! 详情命令：get_log_detail / get_log_context / close_files。

use super::{CmdError, CmdResult};
use crate::state::AppState;
use logscope_core::model::event::EventRef;
use tauri::State;

/// 完整事件详情
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogDetailDto {
    pub file_id: u32,
    pub sequence_no: u32,
    pub timestamp_ms: Option<i64>,
    pub level: String,
    pub truncated: bool,
    pub unknown: bool,
    pub bad_timestamp: bool,
    pub start_offset: u64,
    pub end_offset: u64,
    /// 完整原文（含多行堆栈）
    pub text: String,
}

/// 按 `{fileId, sequenceNo}` 读取完整事件原文。
#[tauri::command]
pub fn get_log_detail(
    file_id: u32,
    sequence_no: u32,
    state: State<'_, AppState>,
) -> CmdResult<LogDetailDto> {
    let ws = state.workspace.read();
    let r = EventRef::new(file_id, sequence_no);
    let entry = ws.index.get(r).ok_or_else(|| {
        CmdError::from(logscope_core::CoreError::InvalidRef {
            file_id,
            sequence_no,
        })
    })?;
    let text = ws
        .readers
        .get(&file_id)
        .map(|reader| reader.lock().read_text(entry))
        .transpose()?
        .unwrap_or_default();
    Ok(LogDetailDto {
        file_id,
        sequence_no,
        timestamp_ms: entry.timestamp_ms,
        level: entry.level.as_str().to_string(),
        truncated: entry.flags.is_truncated(),
        unknown: entry.flags.is_unknown(),
        bad_timestamp: entry.flags.has_bad_timestamp(),
        start_offset: entry.start_offset,
        end_offset: entry.end_offset,
        text,
    })
}

/// 上下文条目
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextItemDto {
    pub sequence_no: u32,
    pub timestamp_ms: Option<i64>,
    pub level: String,
    pub text: String,
}

/// 上下文结果：`mode` 为实际使用的模式（`thread` 同线程 / `physical` 物理顺序）。
/// 请求同线程但事件无线程信息（Raw / 无线程段 / UNKNOWN）时自动回退 physical，
/// 由前端据此提示用户。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextResultDto {
    pub mode: String,
    pub items: Vec<ContextItemDto>,
}

/// 同文件前后文（物理顺序，或 `same_thread=true` 时同线程；无线程信息自动回退物理顺序）。
#[tauri::command]
pub fn get_log_context(
    file_id: u32,
    sequence_no: u32,
    before: Option<u32>,
    after: Option<u32>,
    same_thread: Option<bool>,
    state: State<'_, AppState>,
) -> CmdResult<ContextResultDto> {
    let before = before.unwrap_or(20).min(500);
    let after = after.unwrap_or(20).min(500);
    let same_thread = same_thread.unwrap_or(false);
    let ws = state.workspace.read();
    let idx = ws
        .index
        .file(file_id)
        .cloned()
        .ok_or_else(|| CmdError::new("not-found", format!("文件不存在: {}", file_id)))?;
    let reader = ws
        .readers
        .get(&file_id)
        .cloned()
        .ok_or_else(|| CmdError::new("not-found", format!("文件读取器不存在: {}", file_id)))?;
    let has_thread = same_thread && reader.lock().thread_of_entry(&idx, sequence_no).is_some();
    let (mode, ctx) = if has_thread {
        let items = reader
            .lock()
            .read_context_thread(&idx, sequence_no, before, after)?;
        ("thread", items)
    } else {
        let items = reader
            .lock()
            .read_context(&idx, sequence_no, before, after)?;
        ("physical", items)
    };
    Ok(ContextResultDto {
        mode: mode.to_string(),
        items: ctx
            .into_iter()
            .map(|(e, text)| ContextItemDto {
                sequence_no: e.sequence_no,
                timestamp_ms: e.timestamp_ms,
                level: e.level.as_str().to_string(),
                text,
            })
            .collect(),
    })
}

/// 关闭文件：释放索引与句柄。
#[tauri::command]
pub fn close_files(file_ids: Vec<u32>, state: State<'_, AppState>) -> CmdResult<usize> {
    let mut ws = state.workspace.write();
    let mut remaining: Vec<std::sync::Arc<logscope_core::index::FileIndex>> =
        ws.index.files.values().cloned().collect();
    let before = remaining.len();
    remaining.retain(|f| !file_ids.contains(&f.meta.file_id));
    for id in &file_ids {
        ws.readers.remove(id); // drop DetailReader → 关闭句柄
    }
    let removed = before - remaining.len();
    ws.index = logscope_core::index::build_workspace(remaining);
    Ok(removed)
}
