//! 事件边界状态机：把"行"组装成"事件"。
//!
//! 判定规则：**非 head 即续行**——多行异常堆栈（`at ...`、`Caused by:`、
//! `Suppressed:`、`... 42 more`、空行）天然归并为同一事件。
//!
//! 不变量：事件区间 `[start, end)` 首尾相接覆盖数据起点（BOM 之后）到文件尾，
//! 无任何字节既不属于事件又未计入 UNKNOWN（不静默丢弃）。

use crate::model::event::{EventFlags, LogEntry};
use crate::model::profile::{EventParser, ParsedHead};
use crate::model::stats::ParseStats;
use crate::parser::time::TimeParser;
use crate::scanner::RawLine;

/// 正在累积的事件（InEvent 或 InUnknown 的合一表示：`head == None` 即 UNKNOWN 段）。
struct ActiveEvent {
    start: u64,
    end: u64,
    bytes: u64,
    head: Option<ParsedHead>,
}

/// 事件组装状态机。由 `scanner::scan_file` 逐行驱动。
pub struct ParserMachine<'p> {
    parser: &'p dyn EventParser,
    time: TimeParser,
    state: Option<ActiveEvent>,
    entries: Vec<LogEntry>,
    stats: ParseStats,
    max_event_bytes: u64,
    next_seq: u32,
}

impl<'p> ParserMachine<'p> {
    pub fn new(parser: &'p dyn EventParser, time: TimeParser, max_event_bytes: u64) -> Self {
        ParserMachine {
            parser,
            time,
            state: None,
            entries: Vec::new(),
            stats: ParseStats::default(),
            max_event_bytes,
            next_seq: 0,
        }
    }

    pub fn event_count(&self) -> usize {
        self.entries.len()
    }

    /// 喂入一行。
    pub fn feed_line(&mut self, line: &RawLine<'_>) {
        let content = line.content();
        let line_bytes = line.end - line.start;

        if self.parser.is_head(content) {
            // 关闭当前事件（若有），开启新事件
            self.close_current(false);
            let head = self.parser.parse_head(content, &self.time);
            self.state = Some(ActiveEvent {
                start: line.start,
                end: line.end,
                bytes: line_bytes,
                head: Some(head),
            });
            return;
        }

        // 续行
        match &mut self.state {
            None => {
                // Idle：文件头噪音 → UNKNOWN 段
                self.state = Some(ActiveEvent {
                    start: line.start,
                    end: line.end,
                    bytes: line_bytes,
                    head: None,
                });
            }
            Some(cur) => {
                if cur.bytes + line_bytes > self.max_event_bytes {
                    // 超限：当前事件强制截断关闭，本行开启新 UNKNOWN 段吞后续续行
                    self.close_current(true);
                    self.state = Some(ActiveEvent {
                        start: line.start,
                        end: line.end,
                        bytes: line_bytes,
                        head: None,
                    });
                } else {
                    cur.end = line.end;
                    cur.bytes += line_bytes;
                }
            }
        }
    }

    /// EOF 收尾：关闭悬挂事件，返回事件表与统计。
    pub fn finish(mut self, _file_len: u64) -> (Vec<LogEntry>, ParseStats) {
        self.close_current(false);
        self.stats.events = self.entries.len();
        (self.entries, self.stats)
    }

    /// 关闭当前累积事件并写入索引（截断时置 `truncated` 标志）。
    fn close_current(&mut self, truncated: bool) {
        let Some(ev) = self.state.take() else { return };

        let (level, timestamp_ms, mut flags) = match &ev.head {
            Some(h) => {
                let mut f = EventFlags::default();
                if h.bad_timestamp {
                    f = f.union(EventFlags::bad_timestamp());
                }
                (h.level, h.timestamp_ms, f)
            }
            None => (
                crate::model::level::LogLevel::Unknown,
                None,
                EventFlags::unknown(),
            ),
        };
        if truncated {
            flags = flags.union(EventFlags::truncated());
            self.stats.truncated_events += 1;
        }

        let entry = LogEntry {
            timestamp_ms,
            start_offset: ev.start,
            end_offset: ev.end,
            sequence_no: self.next_seq,
            level,
            flags,
        };
        self.next_seq += 1;
        self.stats.count_level(level);
        self.stats.observe_timestamp(timestamp_ms);
        self.entries.push(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::level::LogLevel;
    use crate::model::profile::{BuiltinProfile, ProfileId};
    use crate::parser::builtin::find_parser;

    fn machine(max_event_bytes: u64) -> ParserMachine<'static> {
        let parser = find_parser(&ProfileId::Builtin(BuiltinProfile::LogbackStd));
        ParserMachine::new(
            parser,
            TimeParser {
                default_tz_offset_min: 480,
            },
            max_event_bytes,
        )
    }

    /// 构造 RawLine（owned 数据模拟借用）
    fn mk_line(data: &[u8], start: u64) -> RawLine<'_> {
        let end = start + data.len() as u64 + 1;
        RawLine {
            bytes: data,
            start,
            end,
            terminated: true,
            truncated: false,
        }
    }

    fn feed_str(m: &mut ParserMachine, lines: &[&str]) {
        let mut off = 0u64;
        for l in lines {
            let line = mk_line(l.as_bytes(), off);
            m.feed_line(&line);
            off = line.end;
        }
    }

    const HEAD1: &str = "2026-11-20 09:00:00.000 [main] INFO  com.example.Foo - first";
    const HEAD2: &str = "2026-11-20 09:00:01.000 [main] ERROR com.example.Foo - second";

    #[test]
    fn multiline_stack_one_event() {
        let mut m = machine(8 * 1024 * 1024);
        feed_str(
            &mut m,
            &[
                HEAD1,
                HEAD2,
                "java.lang.NullPointerException: null",
                "\tat com.example.Foo.bar(Foo.java:42)",
                "\tat com.example.Baz.qux(Baz.java:7)",
                "Caused by: java.lang.IllegalStateException: root",
                "\t... 3 common frames omitted",
            ],
        );
        let (entries, stats) = m.finish(0);
        assert_eq!(entries.len(), 2, "堆栈应归并为 1 个事件");
        assert_eq!(stats.events, 2);
        assert_eq!(entries[0].level, LogLevel::Info);
        assert_eq!(entries[1].level, LogLevel::Error);
        // 事件 2 覆盖头 + 全部续行
        assert_eq!(entries[1].start_offset, entries[0].end_offset);
    }

    #[test]
    fn head_noise_becomes_unknown() {
        let mut m = machine(8 * 1024 * 1024);
        feed_str(
            &mut m,
            &[
                "JVM started",
                "WARNING: incubator",
                HEAD1,
                "tail continuation",
            ],
        );
        let (entries, stats) = m.finish(0);
        // 噪音段(UNKNOWN) + HEAD 事件（尾部续行正确并入）
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].level, LogLevel::Unknown);
        assert!(entries[0].flags.is_unknown());
        assert_eq!(entries[1].level, LogLevel::Info);
        assert!(
            entries[1].byte_len() > HEAD1.len() as u64 + 1,
            "尾部续行应并入 HEAD 事件"
        );
        assert_eq!(stats.unknown_count(), 1);
        // 无缝覆盖
        assert_eq!(entries[0].start_offset, 0);
        assert_eq!(entries[0].end_offset, entries[1].start_offset);
    }

    #[test]
    fn oversized_event_truncates() {
        let mut m = machine(80); // 事件上限 80 字节
        feed_str(
            &mut m,
            &[HEAD1, "java.lang.Exception: boom", "\tat a.b.c(a.java:1)"],
        );
        let (entries, stats) = m.finish(0);
        // 头行本身 < 80 → 事件 1；后续行并入后超限 → 截断关闭，续行进 UNKNOWN
        assert!(entries[0].flags.is_truncated());
        assert_eq!(entries[0].level, LogLevel::Info);
        assert!(entries.len() >= 2);
        assert!(entries[1].flags.is_unknown());
        assert_eq!(stats.truncated_events, 1);
    }

    #[test]
    fn bad_timestamp_keeps_level() {
        let mut m = machine(8 * 1024 * 1024);
        feed_str(
            &mut m,
            &["2026-13-45 09:00:00.000 [main] ERROR c.Foo - bad date"],
        );
        let (entries, stats) = m.finish(0);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, LogLevel::Error);
        assert!(entries[0].timestamp_ms.is_none());
        assert!(entries[0].flags.has_bad_timestamp());
        assert!(!entries[0].flags.is_unknown());
        assert_eq!(stats.no_timestamp_events, 1);
    }

    #[test]
    fn all_bytes_covered_invariant() {
        let mut m = machine(8 * 1024 * 1024);
        let lines = [
            "noise",
            HEAD1,
            "\tstack line",
            HEAD2,
            "Caused by: x",
            "",
            "\t... 1 more",
        ];
        let mut off = 0u64;
        let mut total = 0u64;
        for l in &lines {
            let line = mk_line(l.as_bytes(), off);
            total += line.end - line.start;
            m.feed_line(&line);
            off = line.end;
        }
        let (entries, _) = m.finish(total);
        let mut cursor = 0u64;
        for e in &entries {
            assert_eq!(e.start_offset, cursor, "事件区间必须无缝衔接");
            cursor = e.end_offset;
        }
        assert_eq!(cursor, total, "必须覆盖到文件尾");
    }
}
