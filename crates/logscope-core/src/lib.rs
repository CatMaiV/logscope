//! # logscope-core
//!
//! LogScope 的核心库：本地静态日志文件的扫描、解析、索引与检索。
//!
//! 设计原则（见 `doc/LogScope_开发计划_V1.0.md` §3.1）：
//! - 原文不整体复制到内存：仅保存事件字段、字节偏移和必要索引，详情按需读取。
//! - 以事件为单位：单条异常及其续行视为同一个日志事件。
//! - 一切长操作可取消。
//! - 文件只读：读取前校验文件身份（大小 / mtime），不静默读取已失效的偏移。
//!
//! 本 crate 不依赖 Tauri；进度通过回调上报，取消通过 `AtomicBool` 传入。

pub mod detail;
pub mod index;
pub mod model;
pub mod parser;
pub mod scanner;
pub mod search;
pub mod timeline;

pub use model::error::CoreError;
