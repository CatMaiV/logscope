//! 查询命令：list_files / get_timeline / search_logs。

use super::import::FileSummaryDto;
use super::{CmdError, CmdResult};
use crate::state::AppState;
use logscope_core::model::level::{LevelSet, LogLevel};
use logscope_core::search::{RawSource, SearchQuery, TextMatcher};
use logscope_core::timeline as tl;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
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

/// M1 检索：同步执行 + limit 截断（游标分页与后台任务取消属 M2）。
#[tauri::command]
pub async fn search_logs(
    app: tauri::AppHandle,
    params: SearchParams,
    limit: Option<usize>,
) -> CmdResult<serde_json::Value> {
    let state: State<'_, AppState> = app.state();
    let limit = limit.unwrap_or(200).clamp(1, 1000);

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
    let ws_snapshot = {
        let ws = state.workspace.read();
        let files: HashMap<u32, Arc<Mutex<logscope_core::detail::DetailReader>>> =
            ws.readers.clone();
        let index_files: Vec<Arc<logscope_core::index::FileIndex>> =
            ws.index.files.values().cloned().collect();
        (files, index_files)
    };
    let (readers, index_files) = ws_snapshot;
    let ws_index = logscope_core::index::build_workspace(index_files);

    let cancel = Arc::new(AtomicBool::new(false));
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let mut provider = WorkspaceProvider {
            readers: readers.clone(),
        };
        logscope_core::search::execute(&query, &ws_index, &mut provider, &cancel)
    })
    .await
    .map_err(|e| CmdError::new("join", e.to_string()))??;

    // 摘要：只读 limit 条
    let state2: State<'_, AppState> = app.state();
    let tz = state2.default_tz_offset_min;
    let ws = state2.workspace.read();
    let mut events = Vec::with_capacity(outcome.refs.len().min(limit));
    for r in outcome.refs.iter().take(limit) {
        let Some(entry) = ws.index.get(*r) else {
            continue;
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

    Ok(serde_json::json!({
        "total": outcome.refs.len(),
        "returned": events.len(),
        "scannedBytes": outcome.scanned_bytes,
        "events": events,
        "tzOffsetMin": tz,
    }))
}

/// 多文件原文提供者（锁守卫限制在方法内）。
struct WorkspaceProvider {
    readers: HashMap<u32, Arc<Mutex<logscope_core::detail::DetailReader>>>,
}

impl logscope_core::search::SourceProvider for WorkspaceProvider {
    fn read_and_match(
        &mut self,
        file_id: u32,
        start: u64,
        end: u64,
        matcher: &TextMatcher,
    ) -> std::io::Result<(u64, bool)> {
        let reader = self.readers.get(&file_id).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("文件 {} 无可用读取器（可能已被外部修改）", file_id),
            )
        })?;
        let mut guard = reader.lock();
        let raw = guard.read_range(start, end)?;
        let n = raw.len() as u64;
        Ok((n, matcher.matches(&raw)))
    }
}
