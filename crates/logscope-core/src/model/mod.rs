//! 数据模型：日志级别、事件、文件元数据、解析 Profile 与统计。

pub mod error;
pub mod event;
pub mod file_meta;
pub mod level;
pub mod profile;
pub mod stats;

pub use error::{CoreError, ParseIssue};
pub use event::{EventFlags, EventRef, LogEntry};
pub use file_meta::{FileIdentity, FileMeta, FileState, SourceEncoding};
pub use level::{LevelSet, LogLevel};
pub use profile::{BuiltinProfile, EventParser, ParsedHead, ProfileId, RawParser};
pub use stats::{ParseStats, ProgressFn, ScanCallbacks, ScanOptions, ScanPhase, ScanProgress};
