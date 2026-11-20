//! 核心错误类型。

/// 核心库错误。
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("不支持的编码: {0}（请转存为 UTF-8 或 GBK 后重试）")]
    UnsupportedEncoding(String),

    #[error("操作已取消")]
    Cancelled,

    #[error("源文件已被外部修改或删除: {0}，请重新导入")]
    FileChanged(String),

    #[error("无效的事件引用 file_id={file_id} sequence_no={sequence_no}")]
    InvalidRef { file_id: u32, sequence_no: u32 },

    #[error("文件为空或不包含任何日志事件")]
    EmptyFile,

    #[error("自定义解析正则无效: {0}")]
    InvalidPattern(String),
}

/// 解析过程中的非致命问题（不中断扫描）。
#[derive(Clone, Debug, thiserror::Error)]
pub enum ParseIssue {
    #[error("时间戳字段非法")]
    BadTimestamp,
    #[error("事件超过大小上限（{bytes} 字节），已强制截断")]
    OversizedEvent { bytes: u64 },
    #[error("首个日志头之前的内容")]
    NoHeadBefore,
}
