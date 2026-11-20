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

/// 按 `file_id` 读取原文并应用匹配器（非泛型，保证 dyn 兼容；
/// 方法内部完成"读取 + 匹配"，锁守卫不逃逸出方法）。
pub trait SourceProvider {
    /// 读取 `[start, end)` 原文并匹配。
    /// 返回 `(读取的字节数, 是否命中)`；文件无读取器时返回 `io::Error(NotFound)`。
    fn read_and_match(
        &mut self,
        file_id: u32,
        start: u64,
        end: u64,
        matcher: &TextMatcher,
    ) -> std::io::Result<(u64, bool)>;
}

/// 单文件提供者（CLI / 单文件测试用）。
pub struct SingleSource<'a> {
    pub file_id: u32,
    pub source: &'a mut dyn RawSource,
}

impl SourceProvider for SingleSource<'_> {
    fn read_and_match(
        &mut self,
        file_id: u32,
        start: u64,
        end: u64,
        matcher: &TextMatcher,
    ) -> std::io::Result<(u64, bool)> {
        if file_id != self.file_id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("file_id {} 不在本提供者中", file_id),
            ));
        }
        let raw = self.source.read_range(start, end)?;
        let n = raw.len() as u64;
        Ok((n, matcher.matches(&raw)))
    }
}

/// 检索结果。
pub struct SearchOutcome {
    /// 命中事件（time_order 顺序）
    pub refs: Vec<EventRef>,
    /// 文本匹配阶段读取的原文字节数
    pub scanned_bytes: u64,
}

/// 执行检索。
///
/// 流程（开发计划 §4.4）：
/// 1. 遍历 `time_order` 候选（时间 / 文件 / 级别 = 纯索引判断，零 IO）
/// 2. 需要文本匹配的候选按偏移读原文（每事件一次读取，M2 引入批量优化）
/// 3. 取消：每次文本读取后检查标志
pub fn execute(
    query: &SearchQuery,
    ws: &WorkspaceIndex,
    provider: &mut dyn SourceProvider,
    cancel: &AtomicBool,
) -> Result<SearchOutcome, CoreError> {
    let mut refs = Vec::new();
    let mut scanned_bytes = 0u64;

    for r in &ws.time_order {
        if cancel.load(Ordering::Relaxed) {
            return Err(CoreError::Cancelled);
        }
        let Some(entry) = ws.get(*r) else { continue };

        // 低成本索引过滤
        if let Some(files) = &query.files {
            if !files.contains(&r.file_id) {
                continue;
            }
        }
        if let Some((lo, hi)) = query.time_range {
            match entry.timestamp_ms {
                Some(ts) if ts >= lo && ts <= hi => {}
                _ => continue,
            }
        }
        if let Some(levels) = &query.levels {
            if !levels.contains(entry.level) {
                continue;
            }
        }

        // 文本匹配：按偏移读原文
        if let Some(matcher) = &query.text {
            let (n, matched) = provider
                .read_and_match(r.file_id, entry.start_offset, entry.end_offset, matcher)
                .map_err(CoreError::Io)?;
            scanned_bytes += n;
            if !matched {
                continue;
            }
        }

        refs.push(*r);
    }

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
}
