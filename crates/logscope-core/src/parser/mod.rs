//! 日志解析：时间解析、内置 Logback 解析器、采样识别与事件边界状态机。

pub mod builtin;
pub mod detect;
pub mod machine;
pub mod time;
