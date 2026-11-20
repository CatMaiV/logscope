//! 日志事件：内存中的最小索引单元。

/// 单条日志事件。仅存字段与字节偏移，**不存消息字符串**（原文按需读取）。
///
/// 内存布局（8 字节对齐，共 40 字节）：
/// `Option<i64>` 16B + `start/end` 各 8B + `sequence_no` 4B + `level`/`flags` 2B + 填充 2B。
/// 千万事件约 400MB。P2 优化：时间戳改 sentinel（`i64::MIN`）可压到 32B，对外 API 不变。
#[derive(Clone, Copy, Debug)]
pub struct LogEntry {
    /// UTC epoch 毫秒；`None` 归入"未知时间"分组，不强行虚构时间轴。
    pub timestamp_ms: Option<i64>,
    /// 事件首行行首的文件绝对字节偏移（BOM 之后）。
    pub start_offset: u64,
    /// 最后一个换行符之后；事件字节区间为 `[start_offset, end_offset)`，相邻事件无缝衔接。
    pub end_offset: u64,
    /// 文件内物理序号，等于在 `FileIndex::entries` 中的下标。
    pub sequence_no: u32,
    pub level: crate::model::level::LogLevel,
    pub flags: EventFlags,
}

impl LogEntry {
    pub fn byte_len(&self) -> u64 {
        self.end_offset - self.start_offset
    }
}

/// 跨层唯一的事件标识（过滤/排序后不错位的关键）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EventRef {
    pub file_id: u32,
    pub sequence_no: u32,
}

impl EventRef {
    pub fn new(file_id: u32, sequence_no: u32) -> Self {
        EventRef {
            file_id,
            sequence_no,
        }
    }
}

/// 事件标志位。
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct EventFlags(u8);

impl EventFlags {
    const UNKNOWN: u8 = 1 << 0;
    const TRUNCATED: u8 = 1 << 1;
    const BAD_TIMESTAMP: u8 = 1 << 2;

    /// 结构未识别（UNKNOWN 事件）
    pub fn unknown() -> Self {
        EventFlags(EventFlags::UNKNOWN)
    }

    /// 事件超过 `max_event_bytes` 被强制截断
    pub fn truncated() -> Self {
        EventFlags(EventFlags::TRUNCATED)
    }

    /// 结构匹配但时间非法（如 `2026-13-45`）；事件本身不是 UNKNOWN
    pub fn bad_timestamp() -> Self {
        EventFlags(EventFlags::BAD_TIMESTAMP)
    }

    pub fn is_unknown(&self) -> bool {
        self.0 & Self::UNKNOWN != 0
    }

    pub fn is_truncated(&self) -> bool {
        self.0 & Self::TRUNCATED != 0
    }

    pub fn has_bad_timestamp(&self) -> bool {
        self.0 & Self::BAD_TIMESTAMP != 0
    }

    pub fn union(&self, other: EventFlags) -> Self {
        EventFlags(self.0 | other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::level::LogLevel;

    #[test]
    fn flags_combine() {
        let f = EventFlags::truncated().union(EventFlags::bad_timestamp());
        assert!(f.is_truncated() && f.has_bad_timestamp());
        assert!(!f.is_unknown());
    }

    #[test]
    fn entry_byte_len() {
        let e = LogEntry {
            timestamp_ms: Some(0),
            start_offset: 3,
            end_offset: 10,
            sequence_no: 0,
            level: LogLevel::Info,
            flags: EventFlags::default(),
        };
        assert_eq!(e.byte_len(), 7);
    }
}
