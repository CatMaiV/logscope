//! 时间桶聚合：时间轴数据源。单次图表目标 ≤ ~1500 桶。

use crate::model::event::LogEntry;
use crate::model::level::LogLevel;

/// 目标桶数上限（开发计划 §4.5：不超过约 1,500 个时间桶）
pub const MAX_BUCKETS: usize = 1500;

/// 桶规格。
#[derive(Clone, Copy, Debug)]
pub struct BucketSpec {
    /// 第一桶起点（对齐到桶边界），UTC 毫秒
    pub start_ms: i64,
    /// 每桶时长（毫秒）
    pub bucket_ms: i64,
    /// 桶数
    pub count: usize,
}

/// 单桶各级别计数（下标 = `LogLevel as usize`）。未知时间事件不进桶。
#[derive(Clone, Copy, Default, Debug)]
pub struct BucketCounts {
    pub per_level: [u32; 7],
}

impl BucketCounts {
    pub fn total(&self) -> u32 {
        self.per_level.iter().sum()
    }

    pub fn warn(&self) -> u32 {
        self.per_level[LogLevel::Warn as usize]
    }

    pub fn error(&self) -> u32 {
        self.per_level[LogLevel::Error as usize] + self.per_level[LogLevel::Fatal as usize]
    }
}

/// 按时间跨度自动选择桶宽（1s/10s/1m/10m/1h/6h/1d 取整），保证桶数 ≤ `MAX_BUCKETS`。
pub fn auto_bucket_size(span_ms: i64) -> i64 {
    const SECOND: i64 = 1_000;
    const MINUTE: i64 = 60 * SECOND;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let candidates = [
        SECOND,
        10 * SECOND,
        MINUTE,
        10 * MINUTE,
        HOUR,
        6 * HOUR,
        DAY,
        7 * DAY,
    ];
    for c in candidates {
        if span_ms / c < MAX_BUCKETS as i64 {
            return c;
        }
    }
    // 超长跨度：按最大桶数向上取整桶宽
    div_ceil(span_ms, MAX_BUCKETS as i64)
}

fn div_ceil(a: i64, b: i64) -> i64 {
    if a <= 0 {
        return 1;
    }
    a / b + i64::from(a % b != 0)
}

/// 将事件流聚合为时间桶。越界（早于 `start_ms` 或超出桶数）的事件计入首/末桶
/// 之外的会被丢弃——调用方应保证 `spec` 覆盖事件时间范围。
pub fn aggregate<'a>(
    entries: impl Iterator<Item = &'a LogEntry>,
    spec: &BucketSpec,
) -> Vec<BucketCounts> {
    let mut buckets = vec![BucketCounts::default(); spec.count];
    for e in entries {
        let Some(ts) = e.timestamp_ms else { continue };
        let idx = (ts - spec.start_ms) / spec.bucket_ms;
        if idx < 0 || idx >= spec.count as i64 {
            continue;
        }
        buckets[idx as usize].per_level[e.level as usize] += 1;
    }
    buckets
}

/// 从全局时间范围推导默认 `BucketSpec`。
pub fn spec_for_range(start_ms: i64, end_ms: i64) -> BucketSpec {
    let span = (end_ms - start_ms).max(0);
    let bucket_ms = auto_bucket_size(span.max(1));
    // 起点对齐到桶边界，避免首桶非整
    let aligned_start = start_ms - start_ms.rem_euclid(bucket_ms);
    let count = div_ceil(end_ms - aligned_start, bucket_ms).max(1) as usize;
    BucketSpec {
        start_ms: aligned_start,
        bucket_ms,
        count: count.clamp(1, MAX_BUCKETS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::event::EventFlags;
    use crate::model::level::LogLevel;

    fn entry(ts: Option<i64>, level: LogLevel) -> LogEntry {
        LogEntry {
            timestamp_ms: ts,
            start_offset: 0,
            end_offset: 10,
            sequence_no: 0,
            level,
            flags: EventFlags::default(),
        }
    }

    #[test]
    fn auto_bucket_scales() {
        assert_eq!(auto_bucket_size(60_000), 1_000, "1 分钟 → 1s 桶");
        assert_eq!(auto_bucket_size(10 * 60_000), 1_000);
        // 1 小时用 10s 桶（360 桶）而非 1 分钟（60 桶）——取满足上限的最小档位
        assert_eq!(auto_bucket_size(3_600_000), 10_000);
        // 1 天：1m 桶 = 1441 桶（≤1500，仍取最小满足档位）
        assert_eq!(auto_bucket_size(86_400_000), 60_000);
        // 10 年：超大跨度仍受控
        let ten_years = 10 * 366 * 86_400_000i64;
        let size = auto_bucket_size(ten_years);
        assert!(ten_years / size < MAX_BUCKETS as i64);
    }

    #[test]
    fn aggregate_counts_by_level() {
        let entries = [
            entry(Some(500), LogLevel::Info),
            entry(Some(1_500), LogLevel::Error),
            entry(Some(1_900), LogLevel::Error),
            entry(Some(2_500), LogLevel::Warn),
            entry(None, LogLevel::Unknown), // 不进桶
        ];
        let spec = BucketSpec {
            start_ms: 0,
            bucket_ms: 1_000,
            count: 3,
        };
        let buckets = aggregate(entries.iter(), &spec);
        assert_eq!(buckets.len(), 3);
        assert_eq!(buckets[0].total(), 1);
        assert_eq!(buckets[1].total(), 2);
        assert_eq!(buckets[1].error(), 2);
        assert_eq!(buckets[2].total(), 1);
        assert_eq!(buckets[2].warn(), 1);
    }

    #[test]
    fn out_of_range_dropped() {
        let entries = [
            entry(Some(-5_000), LogLevel::Info),
            entry(Some(50_000), LogLevel::Info),
        ];
        let spec = BucketSpec {
            start_ms: 0,
            bucket_ms: 1_000,
            count: 3,
        };
        let buckets = aggregate(entries.iter(), &spec);
        assert_eq!(buckets.iter().map(|b| b.total()).sum::<u32>(), 0);
    }

    #[test]
    fn spec_for_range_aligns_and_caps() {
        let spec = spec_for_range(1_234, 61_234);
        assert_eq!(spec.bucket_ms, 1_000);
        assert_eq!(spec.start_ms % 1_000, 0);
        assert!(spec.count <= MAX_BUCKETS);
        assert!(spec.count >= 60);
    }
}
