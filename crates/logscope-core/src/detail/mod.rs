//! 原文按需读取：单事件、上下文、摘要。每次读取前校验文件身份。

use crate::index::FileIndex;
use crate::model::error::CoreError;
use crate::model::event::LogEntry;
use crate::model::file_meta::{FileIdentity, FileMeta, SourceEncoding};
use crate::model::profile::{BuiltinProfile, ProfileId};
use crate::parser::builtin::find_parser;
use crate::parser::time::TimeParser;
use crate::scanner::decode_for_display;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

/// 摘要消息头最大字节数
const MSG_HEAD_BYTES: usize = 160;

/// 事件原文读取器（独立持有只读 File 句柄）。
pub struct DetailReader {
    file: File,
    identity: FileIdentity,
    encoding: SourceEncoding,
    profile: Option<ProfileId>,
    default_tz_offset_min: i32,
}

impl DetailReader {
    pub fn open(meta: &FileMeta, default_tz_offset_min: i32) -> Result<Self, CoreError> {
        let file = File::open(&meta.path)?;
        let identity = FileIdentity::capture(&file)?;
        if !identity.matches(&FileIdentity {
            len: meta.file_size,
            mtime: meta.modified_at,
        }) {
            return Err(CoreError::FileChanged(meta.path.display().to_string()));
        }
        Ok(DetailReader {
            file,
            identity,
            encoding: meta.encoding,
            profile: meta.profile,
            default_tz_offset_min,
        })
    }

    /// 每次读取前校验文件长度（O(1)）；失配报 `FileChanged`，不静默读错误原文。
    fn verify_identity(&self) -> Result<(), CoreError> {
        let current = FileIdentity::capture(&self.file)?;
        if !current.matches(&self.identity) {
            return Err(CoreError::FileChanged(format!(
                "文件大小或修改时间已变化（导入时 {} 字节）",
                self.identity.len
            )));
        }
        Ok(())
    }

    /// 读取事件原始字节 `[start, end)`——**逐字节原样**（含尾部换行），
    /// 用于与源文件做 diff 级别的偏移正确性验证。
    pub fn read_raw_exact(&mut self, entry: &LogEntry) -> Result<Vec<u8>, CoreError> {
        self.verify_identity()?;
        let mut buf = vec![0u8; (entry.end_offset - entry.start_offset) as usize];
        self.file.seek(SeekFrom::Start(entry.start_offset))?;
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// 读取事件原文（去除尾部换行符与 `\r`）并按编码解码（展示层）。
    pub fn read_text(&mut self, entry: &LogEntry) -> Result<String, CoreError> {
        let mut raw = self.read_raw_exact(entry)?;
        while matches!(raw.last(), Some(b'\n' | b'\r')) {
            raw.pop();
        }
        Ok(decode_for_display(&raw, self.encoding))
    }

    /// 同文件物理顺序上下文：`before` / `after` 条。
    pub fn read_context(
        &mut self,
        idx: &FileIndex,
        seq: u32,
        before: u32,
        after: u32,
    ) -> Result<Vec<(LogEntry, String)>, CoreError> {
        let slice: Vec<LogEntry> = idx.context_slice(seq, before, after).to_vec();
        let mut out = Vec::with_capacity(slice.len());
        for e in slice {
            let text = self.read_text(&e)?;
            out.push((e, text));
        }
        Ok(out)
    }

    /// 同线程上下文：以当前事件的线程名为准，在物理邻居中筛出同线程的前后各 N 条。
    ///
    /// 实现方式（索引不存线程名，按需提取）：一次性读取当前事件前后扫描窗口的原文，
    /// 逐事件解析首行的 thread 组并比对。找不到线程名（Raw / 无线程段 Profile /
    /// UNKNOWN 事件）时**回退物理顺序上下文**——调用方无法区分两种模式时展示依然合理。
    ///
    /// 扫描窗口上限 `MAX_THREAD_SCAN` 个事件或 `MAX_THREAD_SPAN` 字节（先到为准）。
    pub fn read_context_thread(
        &mut self,
        idx: &FileIndex,
        seq: u32,
        before: u32,
        after: u32,
    ) -> Result<Vec<(LogEntry, String)>, CoreError> {
        const MAX_THREAD_SCAN: usize = 4000;
        const MAX_THREAD_SPAN: u64 = 8 * 1024 * 1024;

        let n = idx.entries.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        let seq = (seq as usize).min(n - 1);

        // 当前事件线程名
        let cur = idx.entries[seq];
        let Some(thread) = self.thread_of(&cur)? else {
            return self.read_context(idx, seq as u32, before, after);
        };

        // 扫描窗口（事件数与字节双上限）
        let mut lo = seq.saturating_sub(MAX_THREAD_SCAN);
        let mut hi = (seq + MAX_THREAD_SCAN + 1).min(n);
        while idx.entries[hi - 1].end_offset - idx.entries[lo].start_offset > MAX_THREAD_SPAN {
            // 双向收缩直到满足字节上限
            let can_shrink_lo = seq - lo > before as usize;
            let can_shrink_hi = hi - 1 - seq > after as usize;
            if !can_shrink_lo && !can_shrink_hi {
                break;
            }
            if can_shrink_lo && (lo < seq) {
                lo += 1;
            }
            if can_shrink_hi && hi - 1 > seq {
                hi -= 1;
            }
        }

        // 一次性读窗口原文
        let span_start = idx.entries[lo].start_offset;
        let span_end = idx.entries[hi - 1].end_offset;
        let buf = crate::search::RawSource::read_range(self, span_start, span_end)?;

        // 逐事件比对线程，收集前后各 N 条（保持物理顺序输出）
        let is_same = |e: &LogEntry| -> bool {
            event_thread_from_buf(&buf, e, span_start, self.profile).as_deref()
                == Some(thread.as_str())
        };
        let mut out: Vec<(LogEntry, String)> = Vec::with_capacity((before + after + 1) as usize);
        // 前 before 条（从近到远收集，再反转）
        let mut before_items: Vec<(LogEntry, String)> = Vec::new();
        for i in (lo..seq).rev() {
            if before_items.len() >= before as usize {
                break;
            }
            let e = idx.entries[i];
            if is_same(&e) {
                before_items.push((e, event_text_from_buf(&buf, &e, span_start, self.encoding)));
            }
        }
        before_items.reverse();
        out.extend(before_items);
        // 当前事件（始终包含）
        out.push((
            cur,
            event_text_from_buf(&buf, &cur, span_start, self.encoding),
        ));
        // 后 after 条
        let limit = before as usize + 1 + after as usize;
        for e in idx.entries[seq + 1..hi].iter().copied() {
            if out.len() >= limit {
                break;
            }
            if is_same(&e) {
                out.push((e, event_text_from_buf(&buf, &e, span_start, self.encoding)));
            }
        }
        Ok(out)
    }

    /// 该事件的线程名（若有）。命令层据此判断"同线程上下文"是否可用。
    pub fn thread_of_entry(&mut self, idx: &FileIndex, seq: u32) -> Option<String> {
        let entry = *idx.entries.get(seq as usize)?;
        self.thread_of(&entry).ok().flatten()
    }

    /// 提取事件首行的线程名（按需，不进索引）
    fn thread_of(&mut self, entry: &LogEntry) -> Result<Option<String>, CoreError> {
        let raw = self.read_raw_exact(entry)?;
        Ok(event_thread_from_buf(
            &raw,
            entry,
            entry.start_offset,
            self.profile,
        ))
    }

    /// 摘要批量读取（M3 虚拟滚动窗口用）：logger + 消息头。
    pub fn read_summaries(
        &mut self,
        idx: &FileIndex,
        seqs: &[u32],
    ) -> Result<Vec<EventSummary>, CoreError> {
        let profile = self
            .profile
            .unwrap_or(ProfileId::Builtin(BuiltinProfile::Raw));
        let parser = find_parser(&profile);
        let time = TimeParser {
            default_tz_offset_min: self.default_tz_offset_min,
        };
        let mut out = Vec::with_capacity(seqs.len());
        for &seq in seqs {
            let Some(entry) = idx.entries.get(seq as usize) else {
                continue;
            };
            let raw = self.read_raw_exact(entry)?;
            let first_line: &[u8] = match raw.iter().position(|b| *b == b'\n') {
                Some(nl) => &raw[..nl],
                None => &raw,
            };
            let first_line = trim_cr(first_line);
            let mut logger = String::new();
            let mut msg_head = decode_for_display(first_line, self.encoding);
            if parser.is_head(first_line) {
                let head = parser.parse_head(first_line, &time);
                if let Some((s, e)) = head.logger {
                    logger = decode_for_display(&first_line[s as usize..e as usize], self.encoding);
                }
            }
            truncate_char_safe(&mut msg_head, MSG_HEAD_BYTES);
            out.push(EventSummary {
                entry: *entry,
                logger,
                msg_head,
            });
        }
        Ok(out)
    }
}

/// `search::RawSource` 实现（检索引擎按偏移读原文）。
impl crate::search::RawSource for DetailReader {
    fn read_range(&mut self, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
        self.verify_identity()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut buf = vec![0u8; (end - start) as usize];
        self.file.seek(SeekFrom::Start(start))?;
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }
}

fn trim_cr(mut line: &[u8]) -> &[u8] {
    while matches!(line.last(), Some(b'\r')) {
        line = &line[..line.len() - 1];
    }
    line
}

/// 事件原文的首行（去 CR）
fn first_line_of(raw: &[u8]) -> &[u8] {
    let end = raw.iter().position(|b| *b == b'\n').unwrap_or(raw.len());
    trim_cr(&raw[..end])
}

/// 从窗口缓冲中取事件首行并解析线程名。
/// `base` 为缓冲 [0] 对应的文件绝对偏移（当缓冲就是整段原文时传 0）。
fn event_thread_from_buf(
    buf: &[u8],
    e: &LogEntry,
    base: u64,
    profile: Option<ProfileId>,
) -> Option<String> {
    let s = (e.start_offset - base) as usize;
    let end = (e.end_offset - base) as usize;
    let raw = buf.get(s..end)?;
    let line = first_line_of(raw);
    let parser = find_parser(&profile.unwrap_or(ProfileId::Builtin(BuiltinProfile::Raw)));
    if !parser.is_head(line) {
        return None;
    }
    let head = parser.parse_head(
        line,
        &TimeParser {
            default_tz_offset_min: 0,
        },
    );
    let (ts, te) = head.thread?;
    Some(String::from_utf8_lossy(&line[ts as usize..te as usize]).into_owned())
}

/// 从窗口缓冲中取事件完整文本（去尾部换行并解码）
fn event_text_from_buf(buf: &[u8], e: &LogEntry, base: u64, encoding: SourceEncoding) -> String {
    let s = (e.start_offset - base) as usize;
    let end = (e.end_offset - base) as usize;
    let mut raw = buf.get(s..end).map(|b| b.to_vec()).unwrap_or_default();
    while matches!(raw.last(), Some(b'\n' | b'\r')) {
        raw.pop();
    }
    decode_for_display(&raw, encoding)
}

/// 按字符边界安全截断字符串（避免 panic）。
fn truncate_char_safe(s: &mut String, max_bytes: usize) {
    if s.len() > max_bytes {
        let mut cut = max_bytes;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
}

/// 列表摘要（不含完整原文）。
pub struct EventSummary {
    pub entry: LogEntry,
    pub logger: String,
    pub msg_head: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::FileIndex;
    use crate::model::stats::{ScanCallbacks, ScanOptions};
    use crate::scanner::scan_file;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    fn scan(path: &Path) -> (FileIndex, crate::scanner::ScanOutput) {
        let cb = ScanCallbacks {
            progress: None,
            cancel: Some(std::sync::Arc::new(AtomicBool::new(false))),
        };
        let out = scan_file(path, &ScanOptions::default(), &cb, 0).unwrap();
        let idx = FileIndex::new(out.meta.clone(), out.entries.clone(), out.stats.clone());
        (idx, out)
    }

    const SAMPLE: &str = "2026-11-20 09:00:00.000 [main] INFO  c.Foo - first\n\
                          2026-11-20 09:00:01.000 [main] ERROR c.Foo - boom\n\
                          java.lang.NullPointerException: null\n\
                          \tat c.Foo.bar(Foo.java:42)\n\
                          2026-11-20 09:00:02.000 [main] WARN  c.Bar - last\n";

    #[test]
    fn read_event_multiline_stack() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        std::fs::write(&p, SAMPLE).unwrap();
        let (idx, out) = scan(&p);
        assert_eq!(idx.entries.len(), 3, "堆栈归并为 1 个事件");

        let mut reader = DetailReader::open(&out.meta, 480).unwrap();
        // 精确字节读取：与源文件区间逐字节一致（偏移正确性核心验证）
        let raw_exact = reader.read_raw_exact(&idx.entries[1]).unwrap();
        let file_bytes = std::fs::read(&p).unwrap();
        let entry = &idx.entries[1];
        assert_eq!(
            &file_bytes[entry.start_offset as usize..entry.end_offset as usize],
            raw_exact.as_slice()
        );
        // 解码文本：含堆栈、无尾部换行
        let text = reader.read_text(&idx.entries[1]).unwrap();
        assert_eq!(
            text,
            "2026-11-20 09:00:01.000 [main] ERROR c.Foo - boom\njava.lang.NullPointerException: null\n\tat c.Foo.bar(Foo.java:42)"
        );
    }

    #[test]
    fn context_physical_order() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        std::fs::write(&p, SAMPLE).unwrap();
        let (idx, out) = scan(&p);
        let mut reader = DetailReader::open(&out.meta, 480).unwrap();
        let ctx = reader.read_context(&idx, 1, 1, 1).unwrap();
        assert_eq!(ctx.len(), 3);
        assert!(ctx[0].1.contains("first"));
        assert!(ctx[1].1.contains("boom"));
        assert!(ctx[2].1.contains("last"));
    }

    #[test]
    fn file_changed_detected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        std::fs::write(&p, SAMPLE).unwrap();
        let (_, out) = scan(&p);
        let mut reader = DetailReader::open(&out.meta, 480).unwrap();
        // 外部修改（追加字节）
        std::fs::write(&p, format!("{}extra\n", SAMPLE)).unwrap();
        let err = reader.read_raw_exact(&out.entries[0]);
        assert!(matches!(err, Err(CoreError::FileChanged(_))));
    }

    #[test]
    fn summaries_extract_logger() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        std::fs::write(&p, SAMPLE).unwrap();
        let (idx, out) = scan(&p);
        let mut reader = DetailReader::open(&out.meta, 480).unwrap();
        let summaries = reader.read_summaries(&idx, &[0, 1, 2]).unwrap();
        assert_eq!(summaries.len(), 3);
        assert_eq!(summaries[0].logger, "c.Foo");
        assert!(summaries[1].msg_head.contains("boom"));
        assert_eq!(summaries[2].logger, "c.Bar");
    }

    #[test]
    fn gbk_event_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        // GBK 编码的中文消息："中文" = D6 D0 CE C4
        let data: Vec<u8> = b"2026-11-20 09:00:00.000 [main] INFO  c.Foo - "
            .iter()
            .chain(&[0xD6, 0xD0, 0xCE, 0xC4])
            .chain(b"\n".iter())
            .copied()
            .collect();
        std::fs::write(&p, &data).unwrap();
        let (idx, out) = scan(&p);
        assert_eq!(out.meta.encoding, SourceEncoding::Gbk);
        let mut reader = DetailReader::open(&out.meta, 480).unwrap();
        let text = reader.read_text(&idx.entries[0]).unwrap();
        assert!(text.contains("中文"), "GBK 应解码为中文: {}", text);
    }

    #[test]
    fn same_thread_context_filters_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("threads.log");
        // 线程交错：main / worker / main / worker / main ...
        let mut buf = String::new();
        let threads = [
            "main", "worker-1", "main", "worker-1", "main", "worker-1", "main",
        ];
        for (i, t) in threads.iter().enumerate() {
            buf.push_str(&format!(
                "2026-11-20 09:00:{:02}.000 [{}] INFO  c.e.T{} - message {} for {}\n",
                i, t, i, i, t
            ));
        }
        std::fs::write(&p, buf).unwrap();
        let (idx, out) = scan(&p);
        assert_eq!(idx.entries.len(), 7);

        let mut reader = DetailReader::open(&out.meta, 480).unwrap();

        // 物理：seq 3 (worker-1) 前后各 2 条 → seq 1..=5（含 main）
        let phys = reader.read_context(&idx, 3, 2, 2).unwrap();
        let seqs: Vec<u32> = phys.iter().map(|(e, _)| e.sequence_no).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4, 5]);

        // 同线程：seq 3 (worker-1) 前后各 2 条 → 只有 worker-1 的 seq 1、3、5
        let same = reader.read_context_thread(&idx, 3, 2, 2).unwrap();
        let seqs2: Vec<u32> = same.iter().map(|(e, _)| e.sequence_no).collect();
        assert_eq!(seqs2, vec![1, 3, 5], "同线程上下文只含 worker-1 事件");
        for (_e, text) in &same {
            assert!(
                text.contains("[worker-1]"),
                "文本应来自 worker 线程: {}",
                text
            );
        }

        // 边界：seq 0 (main) 取前 5 后 5 → main 的全部（0,2,4,6）
        let same2 = reader.read_context_thread(&idx, 0, 5, 5).unwrap();
        let seqs3: Vec<u32> = same2.iter().map(|(e, _)| e.sequence_no).collect();
        assert_eq!(seqs3, vec![0, 2, 4, 6]);

        // 回退：无线程 Profile（Raw）→ 等同物理上下文
        let dir2 = tempfile::tempdir().unwrap();
        let p2 = dir2.path().join("raw.txt");
        std::fs::write(&p2, "plain text\nwithout format\n").unwrap();
        let (idx2, out2) = scan(&p2);
        let mut r2 = DetailReader::open(&out2.meta, 480).unwrap();
        let fb = r2.read_context_thread(&idx2, 0, 1, 1).unwrap();
        let phys2 = r2.read_context(&idx2, 0, 1, 1).unwrap();
        assert_eq!(fb.len(), phys2.len());
    }
}
