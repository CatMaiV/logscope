//! 解析 Profile：内置 Logback 模式标识与事件头解析器抽象。

use crate::model::level::LogLevel;
use crate::parser::time::TimeParser;

/// 内置解析 Profile。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BuiltinProfile {
    /// Spring Boot 默认：`2026-11-20 09:15:42.123  INFO 12345 --- [main] com.foo.Bar : msg`
    SpringBootDefault,
    /// Logback 标准：`%d [%thread] %-5level %logger - %msg`
    LogbackStd,
    /// 无线程段的 Logback 变体：`%d %-5level %logger - %msg`
    LogbackNoThread,
    /// 兜底：不识别任何头，全部作为 UNKNOWN 分段
    Raw,
}

/// Profile 标识（M4 的自定义规则走 `Custom`）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ProfileId {
    Builtin(BuiltinProfile),
    Custom(u32),
}

impl ProfileId {
    pub fn display_name(&self) -> String {
        match self {
            ProfileId::Builtin(b) => match b {
                BuiltinProfile::SpringBootDefault => "Spring Boot 默认".to_string(),
                BuiltinProfile::LogbackStd => "Logback 标准 [thread]".to_string(),
                BuiltinProfile::LogbackNoThread => "Logback 无线程段".to_string(),
                BuiltinProfile::Raw => "原始文本（未识别）".to_string(),
            },
            ProfileId::Custom(id) => format!("自定义 #{id}"),
        }
    }
}

/// 事件头解析结果。时间解析由 `TimeParser` 完成（显式时区优先，否则工作区默认）。
pub struct ParsedHead {
    pub level: LogLevel,
    /// logger 字段在行内的字节区间 `[s, e)`（相对行首），避免字符串分配
    pub logger: Option<(u64, u64)>,
    /// thread 字段在行内的字节区间 `[s, e)`（相对行首）；无线程段的 Profile 为 None
    pub thread: Option<(u64, u64)>,
    pub timestamp_ms: Option<i64>,
    /// 原始时区偏移（分钟），供展示"原始时区"
    pub tz_offset_min: Option<i16>,
    /// 结构匹配但日期/时间非法（如 `2026-13-45`）：保留级别，时间置 None
    pub bad_timestamp: bool,
}

/// 事件头解析器。内置实现见 `parser::builtin`；M4 自定义 Pattern 复用同一 trait。
pub trait EventParser: Send + Sync {
    fn id(&self) -> ProfileId;

    fn name(&self) -> &'static str;

    /// 该行是否为"新日志事件起始行"（快路径，仅 is_match）
    fn is_head(&self, line: &[u8]) -> bool;

    /// 解析起始行字段（内部恰执行一次 captures）
    fn parse_head(&self, line: &[u8], time: &TimeParser) -> ParsedHead;
}

/// Raw 兜底解析器：不识别任何头。
pub struct RawParser;

impl EventParser for RawParser {
    fn id(&self) -> ProfileId {
        ProfileId::Builtin(BuiltinProfile::Raw)
    }

    fn name(&self) -> &'static str {
        "raw"
    }

    fn is_head(&self, line: &[u8]) -> bool {
        // 无格式文件：每行（非空）视为一个独立事件，避免整文件成为单个巨型事件。
        // 事件无级别、无时间，进入"未知时间"分组（仍可搜索、可查看原文）。
        !line.is_empty()
    }

    fn parse_head(&self, _line: &[u8], _time: &TimeParser) -> ParsedHead {
        // 不会被调用：is_head 恒为 false
        ParsedHead {
            level: LogLevel::Unknown,
            logger: None,
            thread: None,
            timestamp_ms: None,
            tz_offset_min: None,
            bad_timestamp: false,
        }
    }
}
