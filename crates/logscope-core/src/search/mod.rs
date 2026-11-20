//! 检索执行：索引过滤（零 IO）+ 原文文本匹配（按偏移读取，可取消）。
//!
//! M1 最小版：文件 / 时间范围 / 级别 / 子串（可选忽略大小写）/ 正则。
//! 结果保持 `time_order` 顺序（天然稳定）。游标分页与后台任务取消属 M2。

use crate::index::WorkspaceIndex;
use crate::model::error::CoreError;
use crate::model::event::EventRef;
use crate::model::level::LevelSet;
use std::sync::atomic::{AtomicBool, Ordering};

/// 文本匹配器。
pub enum TextMatcher {
    Substring {
        needle: Vec<u8>,
        case_insensitive: bool,
    },
    Regex(regex::bytes::Regex),
}

impl TextMatcher {
    /// M1 说明：子串忽略大小写按 ASCII 折叠（中文等非 ASCII 字节不受影响）；
    /// Unicode 感知的大小写请用 `Regex`（regex crate 支持 Unicode case fold）。
    pub fn matches(&self, hay: &[u8]) -> bool {
        match self {
            TextMatcher::Substring {
                needle,
                case_insensitive,
            } => {
                if needle.is_empty() {
                    return true;
                }
                if *case_insensitive {
                    let hay_l: Vec<u8> = hay.iter().map(|b| b.to_ascii_lowercase()).collect();
                    let needle_l: Vec<u8> = needle.iter().map(|b| b.to_ascii_lowercase()).collect();
                    memchr::memmem::find(&hay_l, &needle_l).is_some()
                } else {
                    memchr::memmem::find(hay, needle).is_some()
                }
            }
            TextMatcher::Regex(re) => re.is_match(hay),
        }
    }
}

/// 查询条件（M1 版本；`None` 表示不过滤）。
#[derive(Default)]
pub struct SearchQuery {
    pub files: Option<Vec<u32>>,
    /// `[start_ms, end_ms]` 闭区间
    pub time_range: Option<(i64, i64)>,
    pub levels: Option<LevelSet>,
    pub text: Option<TextMatcher>,
}

/// 原文读取抽象：生产环境由 `DetailReader` 实现，测试可用内存实现。
pub trait RawSource {
    fn read_range(&mut self, start: u64, end: u64) -> std::io::Result<Vec<u8>>;
}

/// 按 `file_id` 读取原文区间（非泛型保证 dyn 兼容；锁守卫限制在方法内）。
/// 匹配逻辑内联在 `execute` 中，provider 只负责字节供给（支持批量读取）。
pub trait SourceProvider {
    /// 读取 `[start, end)` 的原始字节。文件无读取器时返回 `io::Error(NotFound)`。
    fn read_span(&mut self, file_id: u32, start: u64, end: u64) -> std::io::Result<Vec<u8>>;
}

/// 单文件提供者（CLI / 单文件测试用）。
pub struct SingleSource<'a> {
    pub file_id: u32,
    pub source: &'a mut dyn RawSource,
}

impl SourceProvider for SingleSource<'_> {
    fn read_span(&mut self, file_id: u32, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
        if file_id != self.file_id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("file_id {} 不在本提供者中", file_id),
            ));
        }
        self.source.read_range(start, end)
    }
}

/// 检索结果。
pub struct SearchOutcome {
    /// 命中事件（time_order 顺序；无时间过滤时未知时间事件按物理序追加在尾部）
    pub refs: Vec<EventRef>,
    /// 文本匹配阶段读取的原文字节数
    pub scanned_bytes: u64,
}

/// 物化的查询结果集（开发计划 §4.4"形成稳定排序后的结果索引"）。
/// 后台任务一次执行完成后物化，翻页零重扫。
pub struct SearchResultSet {
    pub refs: Vec<EventRef>,
    pub scanned_bytes: u64,
}

impl SearchResultSet {
    pub fn total(&self) -> usize {
        self.refs.len()
    }

    /// 游标分页：`offset` 起取 `limit` 条。返回空切片表示越界。
    pub fn page(&self, offset: usize, limit: usize) -> &[EventRef] {
        self.refs
            .get(offset..)
            .map(|s| s.split_at(limit.min(s.len())).0)
            .unwrap_or(&[])
    }
}

/// 检索进度（节流上报：每 PROGRESS_STEP 个候选或完成时）。
#[derive(Clone, Copy, Debug)]
pub struct SearchProgress {
    /// 已遍历候选事件数
    pub candidates_done: u64,
    /// 候选总数
    pub candidates_total: u64,
    /// 已命中数
    pub matched: u64,
    /// 文本阶段已读取的原文字节数
    pub scanned_bytes: u64,
}

pub type SearchProgressFn = std::sync::Arc<dyn Fn(SearchProgress) + Send + Sync>;

/// 进度节流：每 65536 个候选上报一次。
const PROGRESS_STEP: u64 = 65_536;

/// 执行检索（无进度回调）。
pub fn execute(
    query: &SearchQuery,
    ws: &WorkspaceIndex,
    provider: &mut dyn SourceProvider,
    cancel: &AtomicBool,
) -> Result<SearchOutcome, CoreError> {
    execute_with_progress(query, ws, provider, cancel, None)
}

/// 文本匹配批量读取：候选按物理偏移排序后合并成大块（间隔 ≤ GAP），
/// 每块一次读取，块内逐事件匹配。把 N 次事件读取降为约 文件大小/块大小 次。
const BATCH_GAP: u64 = 256 * 1024;
/// 单块上限（含候选事件本身的最大长度）
const BATCH_MAX: u64 = 8 * 1024 * 1024;

/// 执行检索（完整版）。
///
/// 流程（开发计划 §4.4 + §9"先过滤再批量读取"）：
/// 1. 遍历 `time_order` 候选（时间 / 文件 / 级别 = 纯索引判断，零 IO）
///    ——有文本条件时先记录候选（ref + 偏移），无文本条件直接命中
/// 2. 文本匹配阶段：候选按 `(file_id, start_offset)` 排序合并成读取块，
///    每块一次读取、块内逐事件匹配（大幅减少 syscall）
/// 3. 按原始时间序（+未知时间物理序）输出命中
/// 4. 取消：两个阶段均检查标志，返回 `Err(Cancelled)`
pub fn execute_with_progress(
    query: &SearchQuery,
    ws: &WorkspaceIndex,
    provider: &mut dyn SourceProvider,
    cancel: &AtomicBool,
    progress: Option<&SearchProgressFn>,
) -> Result<SearchOutcome, CoreError> {
    let mut scanned_bytes = 0u64;
    let total = (ws.time_order.len() + ws.unknown_time.len()) as u64;

    let report = |done: u64, matched: u64, scanned: u64| {
        if let Some(f) = progress {
            f(SearchProgress {
                candidates_done: done,
                candidates_total: total,
                matched,
                scanned_bytes: scanned,
            });
        }
    };

    // ---- 阶段 1：索引过滤，产出候选序列（保持输出顺序） ----
    // candidates[i] = (ref, start, end)；hit_flags[i] 对位标记文本命中
    let mut candidates: Vec<(EventRef, u64, u64)> = Vec::new();
    let mut done: u64 = 0;
    let mut last_report: u64 = 0;
    let needs_text = query.text.is_some();

    {
        let mut consider = |r: &EventRef| -> Result<bool, CoreError> {
            if cancel.load(Ordering::Relaxed) {
                return Err(CoreError::Cancelled);
            }
            let Some(entry) = ws.get(*r) else {
                return Ok(false);
            };
            if let Some(files) = &query.files {
                if !files.contains(&r.file_id) {
                    return Ok(false);
                }
            }
            if let Some((lo, hi)) = query.time_range {
                match entry.timestamp_ms {
                    Some(ts) if ts >= lo && ts <= hi => {}
                    _ => return Ok(false),
                }
            }
            if let Some(levels) = &query.levels {
                if !levels.contains(entry.level) {
                    return Ok(false);
                }
            }
            // 候选（也是结果的载体）：无论是否有文本条件都记录
            let _ = needs_text;
            candidates.push((*r, entry.start_offset, entry.end_offset));
            Ok(true) // 索引层通过
        };

        let mut run = |r: &EventRef| -> Result<(), CoreError> {
            consider(r)?;
            done += 1;
            if done - last_report >= PROGRESS_STEP {
                last_report = done;
                report(done, 0, scanned_bytes);
            }
            Ok(())
        };

        for r in &ws.time_order {
            run(r)?;
        }
        // 未知时间事件：仅当查询不含时间过滤时纳入（物理序，位于结果尾部）
        if query.time_range.is_none() {
            for r in &ws.unknown_time {
                run(r)?;
            }
        }
    }

    // ---- 阶段 2：文本批量匹配 ----
    let mut hit_flags = vec![false; candidates.len()];
    let mut matched: u64 = 0;
    if let Some(matcher) = &query.text {
        // 按物理位置排序（稳定排序保持时间序候选的相对顺序，同一位置不变）
        let mut order: Vec<usize> = (0..candidates.len()).collect();
        order.sort_by_key(|&i| (candidates[i].0.file_id, candidates[i].1));

        // 合并成读取块：同文件且与前块末尾间隔 ≤ BATCH_GAP、总长 ≤ BATCH_MAX
        let mut i = 0usize;
        let mut scanned_events: u64 = 0;
        while i < order.len() {
            if cancel.load(Ordering::Relaxed) {
                return Err(CoreError::Cancelled);
            }
            let file_id = candidates[order[i]].0.file_id;
            let span_start = candidates[order[i]].1;
            let mut span_end = candidates[order[i]].2;
            let mut group_end = i + 1;
            while group_end < order.len() {
                let (r2, s2, e2) = candidates[order[group_end]];
                if r2.file_id != file_id
                    || s2 > span_end + BATCH_GAP
                    || (e2 - span_start) > BATCH_MAX
                {
                    break;
                }
                span_end = span_end.max(e2);
                group_end += 1;
            }
            // 一次读取整块
            let buf = provider
                .read_span(file_id, span_start, span_end)
                .map_err(CoreError::Io)?;
            scanned_bytes += buf.len() as u64;
            for &ci in &order[i..group_end] {
                let (_, s, e) = candidates[ci];
                let rel = (s - span_start) as usize;
                let rel_end = (e - span_start) as usize;
                if matcher.matches(&buf[rel..rel_end.min(buf.len())]) {
                    hit_flags[ci] = true;
                    matched += 1;
                }
                scanned_events += 1;
                if scanned_events % PROGRESS_STEP == 0 {
                    report(done, matched, scanned_bytes);
                }
            }
            i = group_end;
        }
    } else {
        // 无文本条件：索引层通过即命中
        for f in hit_flags.iter_mut() {
            *f = true;
        }
        matched = candidates.len() as u64;
    }

    // ---- 阶段 3：按候选原序输出 ----
    let refs: Vec<EventRef> = candidates
        .iter()
        .zip(hit_flags.iter())
        .filter(|(_, hit)| **hit)
        .map(|((r, _, _), _)| *r)
        .collect();

    report(done, matched, scanned_bytes); // 终报
    Ok(SearchOutcome {
        refs,
        scanned_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{build_workspace, FileIndex};
    use crate::model::event::{EventFlags, LogEntry};
    use crate::model::file_meta::{FileMeta, SourceEncoding};
    use crate::model::level::{LevelSet, LogLevel};
    use crate::model::stats::ParseStats;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// 内存原文源：整个文件内容 + 事件表
    struct MemSource {
        data: Vec<u8>,
    }

    impl RawSource for MemSource {
        fn read_range(&mut self, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
            Ok(self.data[start as usize..end as usize].to_vec())
        }
    }

    fn fixture() -> (Arc<FileIndex>, Vec<u8>) {
        // 3 个事件，按时间乱序存放
        let lines: Vec<&[u8]> = vec![
            b"2026-11-20 09:00:01.000 [main] ERROR c.Foo - Connection refused\n",
            b"2026-11-20 09:00:00.000 [main] INFO  c.Foo - started\n",
            b"2026-11-20 09:00:02.000 [main] WARN  c.Bar - slow query \xe7\x94\xa8\xe6\x88\xb7\xe7\x99\xbb\xe5\xbd\x95\xe5\xa4\xb1\xe8\xb4\xa5\n",
        ];
        let mut data = Vec::new();
        let mut starts = Vec::new();
        for l in &lines {
            starts.push(data.len());
            data.extend_from_slice(l);
        }
        let stamps = [1000i64, 0, 2000];
        let levels = [LogLevel::Error, LogLevel::Info, LogLevel::Warn];
        let entries: Vec<LogEntry> = (0..3)
            .map(|i| LogEntry {
                timestamp_ms: Some(stamps[i]),
                start_offset: starts[i] as u64,
                end_offset: starts[i] as u64 + lines[i].len() as u64,
                sequence_no: i as u32,
                level: levels[i],
                flags: EventFlags::default(),
            })
            .collect();
        let meta = FileMeta {
            file_id: 0,
            path: PathBuf::from("t.log"),
            file_size: data.len() as u64,
            modified_at: None,
            encoding: SourceEncoding::Utf8,
            profile: None,
        };
        (
            Arc::new(FileIndex::new(meta, entries, ParseStats::default())),
            data,
        )
    }

    #[test]
    fn index_filters_zero_io() {
        let (idx, data) = fixture();
        let ws = build_workspace(vec![idx]);
        let mut src = MemSource { data };
        let mut provider = SingleSource {
            file_id: 0,
            source: &mut src,
        };
        let cancel = AtomicBool::new(false);

        // 级别过滤
        let q = SearchQuery {
            levels: Some(LevelSet::at_least(LogLevel::Warn)),
            ..Default::default()
        };
        let out = execute(&q, &ws, &mut provider, &cancel).unwrap();
        assert_eq!(out.refs.len(), 2);
        assert!(out.scanned_bytes == 0, "纯索引过滤不应读原文");

        // 时间过滤
        let q2 = SearchQuery {
            time_range: Some((0, 1000)),
            ..Default::default()
        };
        let out2 = execute(&q2, &ws, &mut provider, &cancel).unwrap();
        assert_eq!(out2.refs.len(), 2);

        // 组合
        let q3 = SearchQuery {
            time_range: Some((0, 1000)),
            levels: Some({
                let mut s = LevelSet::empty();
                s.insert(LogLevel::Error);
                s
            }),
            ..Default::default()
        };
        let out3 = execute(&q3, &ws, &mut provider, &cancel).unwrap();
        assert_eq!(out3.refs.len(), 1);
        assert_eq!(out3.refs[0].sequence_no, 0);
    }

    #[test]
    fn text_search_respects_time_order() {
        let (idx, data) = fixture();
        let ws = build_workspace(vec![idx]);
        let mut src = MemSource { data };
        let mut provider = SingleSource {
            file_id: 0,
            source: &mut src,
        };
        let cancel = AtomicBool::new(false);

        let q = SearchQuery {
            text: Some(TextMatcher::Substring {
                needle: b"started".to_vec(),
                case_insensitive: false,
            }),
            ..Default::default()
        };
        let out = execute(&q, &ws, &mut provider, &cancel).unwrap();
        assert_eq!(out.refs.len(), 1);
        assert_eq!(out.refs[0].sequence_no, 1);
        assert!(out.scanned_bytes > 0);
    }

    #[test]
    fn case_insensitive_and_chinese() {
        let (idx, data) = fixture();
        let ws = build_workspace(vec![idx]);
        let mut src = MemSource { data };
        let mut provider = SingleSource {
            file_id: 0,
            source: &mut src,
        };
        let cancel = AtomicBool::new(false);

        let q = SearchQuery {
            text: Some(TextMatcher::Substring {
                needle: b"error".to_vec(),
                case_insensitive: true,
            }),
            ..Default::default()
        };
        let out = execute(&q, &ws, &mut provider, &cancel).unwrap();
        assert_eq!(out.refs.len(), 1);

        let q2 = SearchQuery {
            text: Some(TextMatcher::Substring {
                needle: "用户登录".as_bytes().to_vec(),
                case_insensitive: false,
            }),
            ..Default::default()
        };
        let out2 = execute(&q2, &ws, &mut provider, &cancel).unwrap();
        assert_eq!(out2.refs.len(), 1);
    }

    #[test]
    fn cancel_returns_err() {
        let (idx, data) = fixture();
        let ws = build_workspace(vec![idx]);
        let mut src = MemSource { data };
        let mut provider = SingleSource {
            file_id: 0,
            source: &mut src,
        };
        let cancel = AtomicBool::new(true); // 预先取消
        let q = SearchQuery::default();
        let out = execute(&q, &ws, &mut provider, &cancel);
        assert!(matches!(out, Err(CoreError::Cancelled)));
    }

    #[test]
    fn regex_matcher() {
        let (idx, data) = fixture();
        let ws = build_workspace(vec![idx]);
        let mut src = MemSource { data };
        let mut provider = SingleSource {
            file_id: 0,
            source: &mut src,
        };
        let cancel = AtomicBool::new(false);
        let q = SearchQuery {
            text: Some(TextMatcher::Regex(
                regex::bytes::Regex::new(r"Connection \w+").unwrap(),
            )),
            ..Default::default()
        };
        let out = execute(&q, &ws, &mut provider, &cancel).unwrap();
        assert_eq!(out.refs.len(), 1);
        assert_eq!(out.refs[0].sequence_no, 0);
    }

    /// 含未知时间事件的夹具（3 有时间 + 1 无时间）
    fn fixture_with_unknown() -> (Arc<FileIndex>, Vec<u8>) {
        let lines: Vec<&[u8]> = vec![
            b"2026-11-20 09:00:01.000 [main] ERROR c.Foo - boom\n",
            b"raw noise without timestamp\n",
            b"2026-11-20 09:00:00.000 [main] INFO  c.Foo - started\n",
            b"2026-11-20 09:00:02.000 [main] WARN  c.Bar - slow\n",
        ];
        let mut data = Vec::new();
        let mut starts = Vec::new();
        for l in &lines {
            starts.push(data.len());
            data.extend_from_slice(l);
        }
        let stamps = [Some(1000i64), None, Some(0), Some(2000)];
        let levels = [
            LogLevel::Error,
            LogLevel::Unknown,
            LogLevel::Info,
            LogLevel::Warn,
        ];
        let entries: Vec<LogEntry> = (0..4)
            .map(|i| LogEntry {
                timestamp_ms: stamps[i],
                start_offset: starts[i] as u64,
                end_offset: starts[i] as u64 + lines[i].len() as u64,
                sequence_no: i as u32,
                level: levels[i],
                flags: if i == 1 {
                    EventFlags::unknown()
                } else {
                    EventFlags::default()
                },
            })
            .collect();
        let meta = FileMeta {
            file_id: 0,
            path: PathBuf::from("t.log"),
            file_size: data.len() as u64,
            modified_at: None,
            encoding: SourceEncoding::Utf8,
            profile: None,
        };
        (
            Arc::new(FileIndex::new(meta, entries, ParseStats::default())),
            data,
        )
    }

    #[test]
    fn unknown_time_included_without_time_filter() {
        let (idx, data) = fixture_with_unknown();
        let ws = build_workspace(vec![idx]);
        assert_eq!(ws.unknown_time.len(), 1);
        let mut src = MemSource { data };
        let mut provider = SingleSource {
            file_id: 0,
            source: &mut src,
        };
        let cancel = AtomicBool::new(false);

        // 无时间过滤：未知时间事件（seq 1）纳入，位于结果尾部（物理序在最后？——
        // unknown 追加在 time_order 之后，seq 1 是 unknown 组唯一成员）
        let q = SearchQuery::default();
        let out = execute(&q, &ws, &mut provider, &cancel).unwrap();
        let seqs: Vec<u32> = out.refs.iter().map(|r| r.sequence_no).collect();
        assert_eq!(
            seqs,
            vec![2, 0, 3, 1],
            "时间序(0,1000,2000) + 未知时间物理序尾追"
        );

        // 有时间过滤：未知时间事件被排除
        let q2 = SearchQuery {
            time_range: Some((i64::MIN, i64::MAX)),
            ..Default::default()
        };
        let out2 = execute(&q2, &ws, &mut provider, &cancel).unwrap();
        assert_eq!(out2.refs.len(), 3, "时间过滤（即使全范围）排除未知时间");
    }

    #[test]
    fn result_set_pagination() {
        let (idx, data) = fixture_with_unknown();
        let ws = build_workspace(vec![idx]);
        let mut src = MemSource { data };
        let mut provider = SingleSource {
            file_id: 0,
            source: &mut src,
        };
        let cancel = AtomicBool::new(false);
        let out = execute(&SearchQuery::default(), &ws, &mut provider, &cancel).unwrap();
        let rs = SearchResultSet {
            refs: out.refs,
            scanned_bytes: 0,
        };
        assert_eq!(rs.total(), 4);
        assert_eq!(rs.page(0, 2).len(), 2);
        assert_eq!(rs.page(2, 2).len(), 2);
        assert_eq!(rs.page(0, 3).len(), 3);
        assert_eq!(rs.page(4, 2).len(), 0, "越界返回空");
        assert_eq!(rs.page(3, 10).len(), 1, "末页不足 limit");
        // 分页内容无缝拼接 == 全量
        let joined: Vec<u32> = rs
            .page(0, 3)
            .iter()
            .chain(rs.page(3, 3).iter())
            .map(|r| r.sequence_no)
            .collect();
        assert_eq!(joined, vec![2, 0, 3, 1]);
    }

    #[test]
    fn progress_callback_reports() {
        let (idx, data) = fixture_with_unknown();
        let ws = build_workspace(vec![idx]);
        let mut src = MemSource { data };
        let mut provider = SingleSource {
            file_id: 0,
            source: &mut src,
        };
        let cancel = AtomicBool::new(false);
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let ev2 = events.clone();
        let pf: SearchProgressFn = std::sync::Arc::new(move |p| ev2.lock().unwrap().push(p));
        let q = SearchQuery::default();
        execute_with_progress(&q, &ws, &mut provider, &cancel, Some(&pf)).unwrap();
        let evs = events.lock().unwrap();
        assert!(!evs.is_empty(), "至少终报一次");
        let last = evs.last().unwrap();
        assert_eq!(last.candidates_done, 4);
        assert_eq!(last.candidates_total, 4);
        assert_eq!(last.matched, 4);
    }

    #[test]
    fn cancel_between_batches() {
        // 三个事件物理间隔 > BATCH_GAP → 形成多个读取块；第 1 块读取后置取消标志
        let gap = 400_000u64;
        let mut data = vec![b'.'; (gap * 3) as usize];
        data[..5].copy_from_slice(b"hello");
        data[gap as usize..gap as usize + 5].copy_from_slice(b"world");
        data[(gap * 2) as usize..(gap * 2) as usize + 5].copy_from_slice(b"again");
        let entries: Vec<LogEntry> = (0..3u32)
            .map(|i| LogEntry {
                timestamp_ms: Some(i as i64 * 1000),
                start_offset: gap * i as u64,
                end_offset: gap * i as u64 + 5,
                sequence_no: i,
                level: LogLevel::Info,
                flags: EventFlags::default(),
            })
            .collect();
        let meta = FileMeta {
            file_id: 0,
            path: PathBuf::from("t.log"),
            file_size: data.len() as u64,
            modified_at: None,
            encoding: SourceEncoding::Utf8,
            profile: None,
        };
        let ws = build_workspace(vec![Arc::new(FileIndex::new(
            meta,
            entries,
            ParseStats::default(),
        ))]);

        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        struct CancelAfterFirst {
            data: Vec<u8>,
            cancel: std::sync::Arc<AtomicBool>,
        }
        impl RawSource for CancelAfterFirst {
            fn read_range(&mut self, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
                self.cancel.store(true, Ordering::Release);
                Ok(self.data[start as usize..end as usize].to_vec())
            }
        }
        let mut src = CancelAfterFirst {
            data,
            cancel: cancel.clone(),
        };
        let mut provider = SingleSource {
            file_id: 0,
            source: &mut src,
        };
        let q = SearchQuery {
            text: Some(TextMatcher::Substring {
                needle: b"o".to_vec(),
                case_insensitive: false,
            }),
            ..Default::default()
        };
        let result = execute(&q, &ws, &mut provider, &cancel);
        assert!(
            matches!(result, Err(CoreError::Cancelled)),
            "第 2 个读取块前应被取消（批间取消检查）"
        );
    }
}
