//! 文件元数据、编码与文件身份（外部修改检测基线）。

use std::path::PathBuf;
use std::time::SystemTime;

/// 源文件编码。解析在字节层进行，编码仅在展示层解码时使用。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SourceEncoding {
    Utf8,
    Utf8Bom,
    Gbk,
    /// 非 UTF-8 且无法判定时的兜底：有损解码（替换非法字节）
    LossyUtf8,
}

impl SourceEncoding {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceEncoding::Utf8 => "UTF-8",
            SourceEncoding::Utf8Bom => "UTF-8 BOM",
            SourceEncoding::Gbk => "GBK",
            SourceEncoding::LossyUtf8 => "LOSSY-UTF8",
        }
    }
}

/// 文件导入状态。
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FileState {
    Importing,
    Ready,
    Failed(String),
    Cancelled,
}

/// 文件身份快照：大小 + 修改时间。读取原文前校验，失配则报 `FileChanged`，
/// 绝不静默读取已失效的偏移（开发计划 §3.1、§8）。
#[derive(Clone, Debug)]
pub struct FileIdentity {
    pub len: u64,
    pub mtime: Option<SystemTime>,
}

impl FileIdentity {
    pub fn capture(file: &std::fs::File) -> std::io::Result<FileIdentity> {
        let meta = file.metadata()?;
        Ok(FileIdentity {
            len: meta.len(),
            mtime: meta.modified().ok(),
        })
    }

    /// 当前文件身份是否与快照一致
    pub fn matches(&self, other: &FileIdentity) -> bool {
        self.len == other.len && self.mtime == other.mtime
    }
}

/// 已导入文件的元数据。
#[derive(Clone, Debug)]
pub struct FileMeta {
    pub file_id: u32,
    pub path: PathBuf,
    /// 导入时的文件大小快照
    pub file_size: u64,
    /// 导入时的 mtime 快照（外部修改检测基线）
    pub modified_at: Option<SystemTime>,
    pub encoding: SourceEncoding,
    pub profile: Option<crate::model::profile::ProfileId>,
}

impl FileMeta {
    pub fn file_name(&self) -> &str {
        self.path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<invalid-utf8-name>")
    }
}
