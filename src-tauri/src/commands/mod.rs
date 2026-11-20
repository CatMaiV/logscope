//! Tauri 命令实现。

pub mod detail;
pub mod import;
pub mod query;

/// 命令错误（可序列化到前端）。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CmdError {
    pub code: String,
    pub message: String,
}

impl CmdError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        CmdError {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl From<logscope_core::CoreError> for CmdError {
    fn from(e: logscope_core::CoreError) -> Self {
        let code = match &e {
            logscope_core::CoreError::Cancelled => "cancelled",
            logscope_core::CoreError::FileChanged(_) => "file-changed",
            logscope_core::CoreError::UnsupportedEncoding(_) => "unsupported-encoding",
            logscope_core::CoreError::InvalidRef { .. } => "invalid-ref",
            logscope_core::CoreError::EmptyFile => "empty-file",
            logscope_core::CoreError::InvalidPattern(_) => "invalid-pattern",
            logscope_core::CoreError::Io(_) => "io",
        };
        CmdError::new(code, e.to_string())
    }
}

pub type CmdResult<T> = Result<T, CmdError>;
