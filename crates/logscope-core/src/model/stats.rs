//! 扫描统计、进度回调与扫描选项。

use crate::model::file_meta::SourceEncoding;
use crate::model::level::LogLevel;
use crate::model::profile::ProfileId;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// 扫描阶段。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScanPhase {
    /// 编码 / Profile 采样探测
    Detect,
    /// 逐行索引
    Indexing,
    Done,
    Cancelled,
    Failed,
}

/// 进度事件（节流后上报：每 16MiB 或 4096 行）。
#[derive(Clone, Debug)]
pub struct ScanProgress {
    pub path: Arc<PathBuf>,
    pub phase: ScanPhase,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub events: u64,
}

pub type ProgressFn = Arc<dyn Fn(ScanProgress) + Send + Sync>;

/// 单文件扫描选项。
#[derive(Clone, Debug)]
pub struct ScanOptions {
    /// 指定 Profile；None 则采样自动识别
    pub profile_hint: Option<ProfileId>,
    /// 指定编码；None 则 BOM + 采样自动识别
    pub encoding_hint: Option<SourceEncoding>,
    /// 自定义解析正则（M4）：存在时优先于自动识别。
    /// 必须包含命名组 `ts` 与 `level`；可选 `tz` / `thread` / `logger` / `msg`。
    pub custom_pattern: Option<String>,
    /// 无时区时间戳的工作区默认时区（分钟偏移），默认 +480（Asia/Shanghai）
    pub default_tz_offset_min: i32,
    /// 单事件最大字节数，超出强制截断（防异常格式无限合并），默认 8MiB
    pub max_event_bytes: usize,
    /// 单物理行最大字节数，超出虚拟断行，默认 4MiB
    pub max_line_bytes: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            profile_hint: None,
            encoding_hint: None,
            custom_pattern: None,
            default_tz_offset_min: 480,
            max_event_bytes: 8 * 1024 * 1024,
            max_line_bytes: 4 * 1024 * 1024,
        }
    }
}

/// 扫描回调集合。
#[derive(Default)]
pub struct ScanCallbacks {
    pub progress: Option<ProgressFn>,
    pub cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl ScanCallbacks {
    pub fn should_cancel(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed))
    }
}

/// 单文件解析统计（导入面板与 CLI `parse` 输出）。
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ParseStats {
    pub total_lines: u64,
    pub events: usize,
    /// 按级别计数，下标 = `LogLevel as usize`；UNKNOWN 桶即"解析失败事件数"
    pub level_counts: [u64; 7],
    pub no_timestamp_events: usize,
    pub truncated_events: u32,
    pub time_min_ms: Option<i64>,
    pub time_max_ms: Option<i64>,
    /// 采样命中率（0.0–1.0）
    pub head_match_ratio: f32,
    pub duration: Duration,
}

impl ParseStats {
    pub fn count_level(&mut self, level: crate::model::level::LogLevel) {
        self.level_counts[level as usize] += 1;
    }

    pub fn warn_count(&self) -> u64 {
        self.level_counts[crate::model::level::LogLevel::Warn as usize]
    }

    pub fn error_count(&self) -> u64 {
        self.level_counts[LogLevel::Error as usize] + self.level_counts[LogLevel::Fatal as usize]
    }

    pub fn unknown_count(&self) -> u64 {
        self.level_counts[crate::model::level::LogLevel::Unknown as usize]
    }

    pub fn observe_timestamp(&mut self, ts: Option<i64>) {
        match ts {
            Some(t) => {
                self.time_min_ms = Some(self.time_min_ms.map_or(t, |m: i64| m.min(t)));
                self.time_max_ms = Some(self.time_max_ms.map_or(t, |m: i64| m.max(t)));
            }
            None => self.no_timestamp_events += 1,
        }
    }
}
