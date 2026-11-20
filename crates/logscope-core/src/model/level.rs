//! 日志级别。`Ord` 即严重度顺序（Unknown 最大、排最后），支持 `>= WARN` 式过滤。

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
    Fatal = 5,
    /// 结构未识别的事件（UNKNOWN 分组）
    Unknown = 6,
}

impl LogLevel {
    pub const COUNT: usize = 7;

    /// 字节级匹配（大小写敏感，Logback 输出为大写）
    pub fn from_bytes(s: &[u8]) -> Option<LogLevel> {
        match s {
            b"TRACE" => Some(LogLevel::Trace),
            b"DEBUG" => Some(LogLevel::Debug),
            b"INFO" => Some(LogLevel::Info),
            b"WARN" => Some(LogLevel::Warn),
            b"ERROR" => Some(LogLevel::Error),
            b"FATAL" => Some(LogLevel::Fatal),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "TRACE",
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
            LogLevel::Fatal => "FATAL",
            LogLevel::Unknown => "UNKNOWN",
        }
    }

    fn bit(&self) -> u8 {
        1 << (*self as u8)
    }
}

/// 级别集合（bitflags），用于"级别多选"过滤条件。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct LevelSet(u8);

impl LevelSet {
    pub fn empty() -> Self {
        LevelSet(0)
    }

    pub fn full() -> Self {
        LevelSet(0x7f)
    }

    pub fn insert(&mut self, level: LogLevel) {
        self.0 |= level.bit();
    }

    pub fn remove(&mut self, level: LogLevel) {
        self.0 &= !level.bit();
    }

    pub fn contains(&self, level: LogLevel) -> bool {
        self.0 & level.bit() != 0
    }

    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// 至少达到 `min` 严重度的所有级别
    pub fn at_least(min: LogLevel) -> Self {
        let mut set = LevelSet::empty();
        for l in [
            LogLevel::Trace,
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
            LogLevel::Fatal,
        ] {
            if l >= min {
                set.insert(l);
            }
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_order() {
        assert!(LogLevel::Error > LogLevel::Warn);
        assert!(LogLevel::Warn > LogLevel::Info);
        assert!(LogLevel::Unknown > LogLevel::Fatal);
    }

    #[test]
    fn level_set_ops() {
        let mut s = LevelSet::at_least(LogLevel::Warn);
        assert!(s.contains(LogLevel::Error) && s.contains(LogLevel::Warn));
        assert!(!s.contains(LogLevel::Info));
        s.remove(LogLevel::Warn);
        assert!(!s.contains(LogLevel::Warn));
    }

    #[test]
    fn from_bytes_str() {
        assert_eq!(LogLevel::from_bytes(b"ERROR"), Some(LogLevel::Error));
        assert_eq!(LogLevel::from_bytes(b"error"), None);
        assert_eq!(LogLevel::Error.as_str(), "ERROR");
    }
}
