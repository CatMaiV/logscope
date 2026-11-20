//! 查询命令：list_files / get_timeline / search_logs。

use super::import::FileSummaryDto;
use super::{CmdError, CmdResult};
use crate::state::AppState;
use logscope_core::model::level::{LevelSet, LogLevel};
use logscope_core::search::{RawSource, SearchQuery, TextMatcher};
use logscope_core::timeline as tl;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{Manager, State};

/// 列出已导入文件。
#[tauri::command]
pub fn list_files(state: State<'_, AppState>) -> CmdResult<Vec<FileSummaryDto>> {
    let ws = state.workspace.read();
    let mut out: Vec<FileSummaryDto> = ws
        .index
        .files
        .values()
        .map(|f| super::import::file_summary(&f.meta, &f.stats))
        .collect();
    out.sort_by_key(|f| f.file_id);
    Ok(out)
}

/// 时间桶请求
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TimelineRequest {
    /// 空 = 全部文件
    pub file_ids: Option<Vec<u32>>,
    /// [startMs, endMs] 闭区间；空 = 全时间范围
    pub time_range: Option<(i64, i64)>,
    /// 显式桶宽（毫秒）；空 = 自动
    pub bucket_ms: Option<i64>,
}

/// 时间桶响应
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineDto {
    pub start_ms: i64,
    pub bucket_ms: i64,
    pub count: usize,
    /// 每桶 [total, warn, error]
    pub buckets: Vec<[u32; 3]>,
    pub total_events: u64,
}

/// 时间轴聚合（服务端聚合，≤1500 桶，不渲染每事件）。
#[tauri::command]
pub fn get_timeline(req: TimelineRequest, state: State<'_, AppState>) -> CmdResult<TimelineDto> {
    let ws = state.workspace.read();

    // 候选：选中文件的全部条目（迭代器装箱）
    let files: Vec<Arc<logscope_core::index::FileIndex>> = match &req.file_ids {
        Some(ids) => ids
            .iter()
            .filter_map(|id| ws.index.files.get(id).cloned())
            .collect(),
        None => ws.index.files.values().cloned().collect(),
    };
    if files.is_empty() {
        return Ok(TimelineDto {
            start_ms: 0,
            bucket_ms: 1_000,
            count: 0,
            buckets: Vec::new(),
            total_events: 0,
        });
    }

    // 时间范围
    let (lo, hi) = match req.time_range {
        Some(r) => r,
        None => {
            let mut lo = i64::MAX;
            let mut hi = i64::MIN;
            for f in &files {
                for e in &f.entries {
                    if let Some(ts) = e.timestamp_ms {
                        lo = lo.min(ts);
                        hi = hi.max(ts);
                    }
                }
            }
            if lo > hi {
                return Ok(TimelineDto {
                    start_ms: 0,
                    bucket_ms: 1_000,
                    count: 0,
                    buckets: Vec::new(),
                    total_events: 0,
                });
            }
            (lo, hi)
        }
    };

    let spec = match req.bucket_ms {
        Some(ms) if ms > 0 => {
            let span = (hi - lo).max(1);
            let count = (span / ms + 1).min(tl::MAX_BUCKETS as i64) as usize;
            tl::BucketSpec {
                start_ms: lo,
                bucket_ms: ms,
                count: count.max(1),
            }
        }
        _ => tl::spec_for_range(lo, hi),
    };

    let mut buckets = vec![[0u32; 3]; spec.count];
    let mut total = 0u64;
    for f in &files {
        for e in &f.entries {
            let Some(ts) = e.timestamp_ms else { continue };
            // 时间范围过滤（桶 spec 起点可能早于 lo）
            if ts < lo || ts > hi {
                continue;
            }
            let idx = ((ts - spec.start_ms) / spec.bucket_ms) as i64;
            if idx < 0 || idx >= spec.count as i64 {
                continue;
            }
            let b = &mut buckets[idx as usize];
            b[0] = b[0].saturating_add(1);
            if e.level == LogLevel::Warn {
                b[1] = b[1].saturating_add(1);
            }
            if e.level >= LogLevel::Error {
                b[2] = b[2].saturating_add(1);
            }
            total += 1;
        }
    }

    Ok(TimelineDto {
        start_ms: spec.start_ms,
        bucket_ms: spec.bucket_ms,
        count: spec.count,
        buckets,
        total_events: total,
    })
}

/// 检索参数
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchParams {
    pub file_ids: Option<Vec<u32>>,
    pub time_range: Option<(i64, i64)>,
    /// 级别集合，如 ["INFO","WARN","ERROR"]
    pub levels: Option<Vec<String>>,
    pub text: Option<String>,
    pub case_insensitive: bool,
    pub regex: bool,
}

/// 检索结果条目（摘要）
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDto {
    pub file_id: u32,
    pub sequence_no: u32,
    pub timestamp_ms: Option<i64>,
    pub level: String,
    pub truncated: bool,
    pub unknown: bool,
    pub byte_len: u64,
    /// 首行摘要（服务端按需读取）
    pub head: String,
}

/// 检索进度事件（Channel 推送）
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchProgressDto {
    pub search_id: String,
    /// running | done | cancelled | failed
    pub phase: String,
    pub matched: u64,
    pub candidates_done: u64,
    pub candidates_total: u64,
    pub scanned_bytes: u64,
    pub message: Option<String>,
}

/// M2 检索：后台任务执行 + Channel 进度 + 可取消。
/// 完成后结果物化为 `SearchResultSet`，用 `search_page` 游标分页读取。
#[tauri::command]
pub async fn search_logs(
    app: tauri::AppHandle,
    params: SearchParams,
    on_progress: tauri::ipc::Channel<SearchProgressDto>,
) -> CmdResult<String> {
    let state: State<'_, AppState> = app.state();

    let levels = params.levels.as_ref().map(|ls| {
        let mut set = LevelSet::empty();
        for l in ls {
            if let Some(lv) = LogLevel::from_bytes(l.to_ascii_uppercase().as_bytes()) {
                set.insert(lv);
            }
        }
        set
    });

    let text = match &params.text {
        Some(t) if params.regex => Some(TextMatcher::Regex(regex::bytes::Regex::new(t).map_err(
            |e| {
                CmdError::new(
                    "bad-regex",
                    format!("正则错误: {}（不支持前后向断言/反向引用）", e),
                )
            },
        )?)),
        Some(t) => Some(TextMatcher::Substring {
            needle: t.clone().into_bytes(),
            case_insensitive: params.case_insensitive,
        }),
        None => None,
    };

    let query = SearchQuery {
        files: params.file_ids.clone(),
        time_range: params.time_range,
        levels,
        text,
    };

    // 快照（避免跨 await 持锁）
    let (readers, index_files) = {
        let ws = state.workspace.read();
        (
            ws.readers.clone(),
            ws.index.files.values().cloned().collect::<Vec<_>>(),
        )
    };
    if index_files.is_empty() {
        return Err(CmdError::new("no-files", "工作区没有已导入的文件"));
    }

    let search_id = uuid::Uuid::new_v4().to_string();
    let handle = Arc::new(crate::state::SearchHandle::new(search_id.clone()));
    state.register_search(handle.clone());

    let app2 = app.clone();
    let sid = search_id.clone();
    let sid2 = search_id.clone();
    tauri::async_runtime::spawn(async move {
        *handle.status.lock() = crate::state::TaskStatus::Running;
        let ws_index = logscope_core::index::build_workspace(index_files);
        let cancel = handle.cancel.clone();
        let ch = on_progress.clone();

        let progress_fn: logscope_core::search::SearchProgressFn = Arc::new(move |p| {
            let _ = ch.send(SearchProgressDto {
                search_id: sid.clone(),
                phase: "running".into(),
                matched: p.matched,
                candidates_done: p.candidates_done,
                candidates_total: p.candidates_total,
                scanned_bytes: p.scanned_bytes,
                message: None,
            });
        });

        let result = tauri::async_runtime::spawn_blocking(move || {
            let mut provider = WorkspaceProvider {
                readers: readers.clone(),
            };
            logscope_core::search::execute_with_progress(
                &query,
                &ws_index,
                &mut provider,
                &cancel,
                Some(&progress_fn),
            )
        })
        .await;

        let final_phase;
        match result {
            Ok(Ok(outcome)) => {
                let total = outcome.refs.len() as u64;
                *handle.result.lock() = Some(logscope_core::search::SearchResultSet {
                    refs: outcome.refs,
                    scanned_bytes: outcome.scanned_bytes,
                });
                *handle.status.lock() = crate::state::TaskStatus::Done;
                final_phase = "done".to_string();
                let _ = on_progress.send(SearchProgressDto {
                    search_id: sid2.clone(),
                    phase: final_phase,
                    matched: total,
                    candidates_done: total,
                    candidates_total: total,
                    scanned_bytes: 0,
                    message: None,
                });
            }
            Ok(Err(logscope_core::CoreError::Cancelled)) => {
                *handle.status.lock() = crate::state::TaskStatus::Cancelled;
                let _ = on_progress.send(SearchProgressDto {
                    search_id: sid2.clone(),
                    phase: "cancelled".into(),
                    matched: 0,
                    candidates_done: 0,
                    candidates_total: 0,
                    scanned_bytes: 0,
                    message: None,
                });
            }
            Ok(Err(e)) => {
                *handle.status.lock() = crate::state::TaskStatus::Failed(e.to_string());
                let _ = on_progress.send(SearchProgressDto {
                    search_id: sid2.clone(),
                    phase: "failed".into(),
                    matched: 0,
                    candidates_done: 0,
                    candidates_total: 0,
                    scanned_bytes: 0,
                    message: Some(e.to_string()),
                });
            }
            Err(join_err) => {
                *handle.status.lock() = crate::state::TaskStatus::Failed(join_err.to_string());
            }
        }
        let _ = app2;
    });

    Ok(search_id)
}

/// 查询检索任务状态（含总数；运行中 total 为已命中数）。
#[tauri::command]
pub fn get_search_status(
    search_id: String,
    state: State<'_, AppState>,
) -> CmdResult<serde_json::Value> {
    let h = state
        .get_search(&search_id)
        .ok_or_else(|| CmdError::new("not-found", format!("检索任务不存在: {}", search_id)))?;
    let status = match h.status() {
        crate::state::TaskStatus::Pending => "pending",
        crate::state::TaskStatus::Running => "running",
        crate::state::TaskStatus::Done => "done",
        crate::state::TaskStatus::Cancelled => "cancelled",
        crate::state::TaskStatus::Failed(_) => "failed",
    };
    let total = h.result.lock().as_ref().map(|rs| rs.total()).unwrap_or(0);
    Ok(serde_json::json!({
        "searchId": search_id,
        "status": status,
        "total": total,
    }))
}

/// 游标分页读取检索结果（零重扫；摘要按需读取当前页）。
#[tauri::command]
pub fn search_page(
    search_id: String,
    offset: Option<usize>,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> CmdResult<serde_json::Value> {
    let h = state
        .get_search(&search_id)
        .ok_or_else(|| CmdError::new("not-found", format!("检索任务不存在: {}", search_id)))?;
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(200).clamp(1, 1000);

    let page_refs: Vec<logscope_core::model::event::EventRef> = {
        let guard = h.result.lock();
        match guard.as_ref() {
            Some(rs) => rs.page(offset, limit).to_vec(),
            None => {
                let st = h.status();
                let msg = match st {
                    crate::state::TaskStatus::Running | crate::state::TaskStatus::Pending => {
                        "检索仍在执行中"
                    }
                    _ => "检索未完成（已取消或失败），无结果可分页",
                };
                return Err(CmdError::new("not-ready", msg));
            }
        }
    };

    let ws = state.workspace.read();
    let mut events = Vec::with_capacity(page_refs.len());
    for r in &page_refs {
        let Some(entry) = ws.index.get(*r) else {
            continue; // 文件已被 close：跳过悬挂引用
        };
        let head = ws
            .readers
            .get(&r.file_id)
            .and_then(|reader| {
                let mut guard = reader.try_lock()?;
                guard.read_text(entry).ok()
            })
            .and_then(|t| t.lines().next().map(|s| s.to_string()))
            .unwrap_or_default();
        events.push(EventDto {
            file_id: r.file_id,
            sequence_no: r.sequence_no,
            timestamp_ms: entry.timestamp_ms,
            level: entry.level.as_str().to_string(),
            truncated: entry.flags.is_truncated(),
            unknown: entry.flags.is_unknown(),
            byte_len: entry.byte_len(),
            head,
        });
    }
    let total = h.result.lock().as_ref().map(|rs| rs.total()).unwrap_or(0);
    let has_more = offset + events.len() < total;
    Ok(serde_json::json!({
        "total": total,
        "offset": offset,
        "returned": events.len(),
        "hasMore": has_more,
        "events": events,
        "tzOffsetMin": state.default_tz_offset_min,
    }))
}

/// 取消正在执行的检索。
#[tauri::command]
pub fn cancel_search(search_id: String, state: State<'_, AppState>) -> CmdResult<bool> {
    let h = state
        .get_search(&search_id)
        .ok_or_else(|| CmdError::new("not-found", format!("检索任务不存在: {}", search_id)))?;
    Ok(h.cancel.swap(true, std::sync::atomic::Ordering::Release))
}

/// 释放检索结果集（前端关闭结果视图时调用）。
#[tauri::command]
pub fn close_search(search_id: String, state: State<'_, AppState>) -> CmdResult<bool> {
    Ok(state.remove_search(&search_id).is_some())
}

/// 多文件原文提供者（锁守卫限制在方法内）。
struct WorkspaceProvider {
    readers: HashMap<u32, Arc<Mutex<logscope_core::detail::DetailReader>>>,
}

impl logscope_core::search::SourceProvider for WorkspaceProvider {
    fn read_span(&mut self, file_id: u32, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
        let reader = self.readers.get(&file_id).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("文件 {} 无可用读取器（可能已被外部修改）", file_id),
            )
        })?;
        let mut guard = reader.lock();
        guard.read_range(start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 契约测试：DTO 序列化键名必须是 camelCase（前端 TS 接口按 camelCase 读取；
    /// 曾因 snake/camel 不一致导致时间戳全空、时间轴不渲染、过滤条件被静默丢弃）。
    #[test]
    fn dto_keys_are_camelcase() {
        let dto = EventDto {
            file_id: 1,
            sequence_no: 2,
            timestamp_ms: Some(3),
            level: "INFO".into(),
            truncated: false,
            unknown: false,
            byte_len: 10,
            head: String::new(),
        };
        let v = serde_json::to_value(&dto).unwrap();
        for key in ["fileId", "sequenceNo", "timestampMs", "byteLen"] {
            assert!(
                v.get(key).is_some(),
                "EventDto 缺少 camelCase 键 {}（实际 {:?}）",
                key,
                v
            );
        }
        for bad in ["file_id", "sequence_no", "timestamp_ms", "byte_len"] {
            assert!(
                v.get(bad).is_none(),
                "EventDto 不应出现 snake_case 键 {}",
                bad
            );
        }

        let tl = TimelineDto {
            start_ms: 0,
            bucket_ms: 1,
            count: 0,
            buckets: Vec::new(),
            total_events: 0,
        };
        let v2 = serde_json::to_value(&tl).unwrap();
        for key in ["startMs", "bucketMs", "totalEvents"] {
            assert!(v2.get(key).is_some(), "TimelineDto 缺少 {}", key);
        }

        let sp = SearchProgressDto {
            search_id: String::new(),
            phase: "done".into(),
            matched: 0,
            candidates_done: 0,
            candidates_total: 0,
            scanned_bytes: 0,
            message: None,
        };
        let v3 = serde_json::to_value(&sp).unwrap();
        for key in [
            "searchId",
            "candidatesDone",
            "candidatesTotal",
            "scannedBytes",
        ] {
            assert!(v3.get(key).is_some(), "SearchProgressDto 缺少 {}", key);
        }
    }
}
