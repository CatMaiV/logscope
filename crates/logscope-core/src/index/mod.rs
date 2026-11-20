//! 内存索引：文件内事件表与跨文件全局时间视图。

use crate::model::event::{EventRef, LogEntry};
use crate::model::file_meta::FileMeta;
use crate::model::stats::ParseStats;
use std::collections::HashMap;
use std::sync::Arc;

/// 单文件索引：事件按文件内物理顺序存放（`sequence_no` 即下标）。
pub struct FileIndex {
    pub meta: FileMeta,
    pub entries: Vec<LogEntry>,
    pub stats: ParseStats,
}

impl FileIndex {
    pub fn new(meta: FileMeta, entries: Vec<LogEntry>, stats: ParseStats) -> Self {
        FileIndex {
            meta,
            entries,
            stats,
        }
    }

    /// 校验引用并取事件
    pub fn get(&self, r: EventRef) -> Option<&LogEntry> {
        if r.file_id != self.meta.file_id {
            return None;
        }
        self.entries.get(r.sequence_no as usize)
    }

    /// 物理上下文：`entries[seq-before ..= seq+after]`，钳制边界。
    /// `before` / `after` 上限各 500。
    pub fn context_slice(&self, seq: u32, before: u32, after: u32) -> &[LogEntry] {
        const MAX_CTX: u32 = 500;
        let n = self.entries.len();
        let seq = (seq as usize).min(n.saturating_sub(1));
        let start = seq.saturating_sub(before.min(MAX_CTX) as usize);
        let end = (seq + after.min(MAX_CTX) as usize + 1).min(n);
        &self.entries[start..end]
    }
}

/// 跨文件全局视图。
pub struct WorkspaceIndex {
    pub files: HashMap<u32, Arc<FileIndex>>,
    /// 按 `(timestamp, file_id, sequence_no)` 稳定排序；未知时间不在其中
    pub time_order: Vec<EventRef>,
    /// 未知时间事件（保持物理顺序）
    pub unknown_time: Vec<EventRef>,
}

impl WorkspaceIndex {
    pub fn file(&self, file_id: u32) -> Option<&Arc<FileIndex>> {
        self.files.get(&file_id)
    }

    pub fn get(&self, r: EventRef) -> Option<&LogEntry> {
        self.files.get(&r.file_id).and_then(|f| f.get(r))
    }

    /// 全部事件数
    pub fn total_events(&self) -> usize {
        self.time_order.len() + self.unknown_time.len()
    }

    /// 全局时间范围（不含未知时间事件）
    pub fn time_range(&self) -> Option<(i64, i64)> {
        let first = self
            .time_order
            .first()
            .and_then(|r| self.get(*r)?.timestamp_ms);
        let last = self
            .time_order
            .last()
            .and_then(|r| self.get(*r)?.timestamp_ms);
        match (first, last) {
            (Some(f), Some(l)) => Some((f, l)),
            _ => None,
        }
    }
}

/// 构建全局索引：各文件物理有序但时间可乱序，故整体稳定排序。
/// 排序键 `(ts || i64::MAX, file_id, sequence_no)`——同毫秒事件按文件、序号稳定排列。
pub fn build_workspace(files: Vec<Arc<FileIndex>>) -> WorkspaceIndex {
    let mut map = HashMap::with_capacity(files.len());
    let mut timed: Vec<(i64, u32, u32)> = Vec::new();
    let mut unknown_time = Vec::new();

    for f in files {
        let file_id = f.meta.file_id;
        for e in &f.entries {
            let r = EventRef::new(file_id, e.sequence_no);
            match e.timestamp_ms {
                Some(ts) => timed.push((ts, file_id, e.sequence_no)),
                None => unknown_time.push(r),
            }
        }
        map.insert(file_id, f);
    }

    // 稳定排序：sort_by_key 稳定，相同键保持插入序（文件序）
    #[cfg(feature = "rayon")]
    timed.par_sort_unstable_by_key(|k| (k.0, k.1, k.2));
    #[cfg(not(feature = "rayon"))]
    timed.sort_unstable_by_key(|k| (k.0, k.1, k.2));

    WorkspaceIndex {
        time_order: timed
            .into_iter()
            .map(|(_ts, file_id, sequence_no)| EventRef {
                file_id,
                sequence_no,
            })
            .collect(),
        unknown_time,
        files: map,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::event::{EventFlags, LogEntry};
    use crate::model::file_meta::{FileMeta, SourceEncoding};
    use crate::model::level::LogLevel;
    use crate::model::stats::ParseStats;
    use std::path::PathBuf;

    fn mk_index(file_id: u32, stamps: &[Option<i64>]) -> Arc<FileIndex> {
        let entries: Vec<LogEntry> = stamps
            .iter()
            .enumerate()
            .map(|(i, ts)| LogEntry {
                timestamp_ms: *ts,
                start_offset: i as u64 * 10,
                end_offset: i as u64 * 10 + 10,
                sequence_no: i as u32,
                level: LogLevel::Info,
                flags: EventFlags::default(),
            })
            .collect();
        let meta = FileMeta {
            file_id,
            path: PathBuf::from(format!("f{}.log", file_id)),
            file_size: stamps.len() as u64 * 10,
            modified_at: None,
            encoding: SourceEncoding::Utf8,
            profile: None,
        };
        Arc::new(FileIndex::new(meta, entries, ParseStats::default()))
    }

    #[test]
    fn cross_file_same_ms_stable_order() {
        // 同毫秒交错：a(100,100,200) b(100,300)
        let a = mk_index(1, &[Some(100), Some(100), Some(200)]);
        let b = mk_index(2, &[Some(100), Some(300)]);
        let ws = build_workspace(vec![a, b]);
        let order: Vec<(u32, u32)> = ws
            .time_order
            .iter()
            .map(|r| (r.file_id, r.sequence_no))
            .collect();
        // (100,1,0) (100,1,1) (100,2,0) (200,1,2) (300,2,1)
        assert_eq!(
            order,
            vec![(1, 0), (1, 1), (2, 0), (1, 2), (2, 1)],
            "同毫秒按 (file_id, seq) 稳定排序"
        );
    }

    #[test]
    fn unknown_time_grouped_separately() {
        let a = mk_index(1, &[Some(50), None, Some(40)]);
        let ws = build_workspace(vec![a]);
        assert_eq!(ws.time_order.len(), 2);
        assert_eq!(ws.unknown_time.len(), 1);
        // 乱序时间已排序
        assert_eq!(
            ws.time_order
                .iter()
                .filter_map(|r| ws.get(*r).and_then(|e| e.timestamp_ms))
                .collect::<Vec<_>>(),
            vec![40, 50]
        );
    }

    #[test]
    fn context_slice_clamped() {
        let a = mk_index(1, &[Some(1), Some(2), Some(3), Some(4), Some(5)]);
        let ws = build_workspace(vec![a.clone()]);
        let idx = ws.file(1).unwrap();
        let s = idx.context_slice(0, 3, 2);
        assert_eq!(s.len(), 3, "前边界钳制");
        let s2 = idx.context_slice(4, 2, 3);
        assert_eq!(s2.len(), 3, "后边界钳制");
    }

    #[test]
    fn get_validates_file_id() {
        let a = mk_index(1, &[Some(1)]);
        let ws = build_workspace(vec![a]);
        assert!(ws.get(EventRef::new(1, 0)).is_some());
        assert!(ws.get(EventRef::new(2, 0)).is_none());
        assert!(ws.get(EventRef::new(1, 99)).is_none());
    }

    #[test]
    fn time_range() {
        let a = mk_index(1, &[Some(40), None, Some(60)]);
        let ws = build_workspace(vec![a]);
        assert_eq!(ws.time_range(), Some((40, 60)));
        let b = mk_index(2, &[None, None]);
        let ws2 = build_workspace(vec![b]);
        assert_eq!(ws2.time_range(), None);
    }
}
