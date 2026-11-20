//! 文件扫描：只读打开、编码探测、Profile 采样识别、逐行索引编排。
//!
//! 本模块**不做语义解析**——行内容交给 `parser::ParserMachine`。

mod encoding;
mod reader;

pub use crate::model::file_meta::FileIdentity;
pub use encoding::{decode_for_display, detect_bom, refine_encoding, utf8_health, Utf8Health};
pub use reader::{LineScanner, RawLine};

use crate::model::error::CoreError;
use crate::model::event::LogEntry;
use crate::model::file_meta::{FileMeta, SourceEncoding};
use crate::model::profile::{BuiltinProfile, ProfileId};
use crate::model::stats::{ParseStats, ScanCallbacks, ScanOptions, ScanPhase, ScanProgress};
use crate::parser::builtin;
use crate::parser::detect;
use crate::parser::machine::ParserMachine;
use crate::parser::time::TimeParser;
use std::path::Path;
use std::sync::Arc;

/// 扫描结果：文件元数据、事件表与统计。
pub struct ScanOutput {
    pub meta: FileMeta,
    pub entries: Vec<LogEntry>,
    pub stats: ParseStats,
}

/// 进度节流参数：每 16MiB 或 4096 行上报一次。
const PROGRESS_BYTES_STEP: u64 = 16 * 1024 * 1024;
const PROGRESS_LINES_STEP: u64 = 4096;

/// 扫描单个静态日志文件（同步、CPU 密集、可取消）。
///
/// - 只读共享打开；`File` 随函数结束 drop 关闭，取消不残留活动状态。
/// - 取消：每块（约 1MiB）检查一次取消标志，返回 `Err(Cancelled)`。
/// - 失败：返回 `Err`，不产生半成品索引。
pub fn scan_file(
    path: &Path,
    opts: &ScanOptions,
    cb: &ScanCallbacks,
    file_id: u32,
) -> Result<ScanOutput, CoreError> {
    let started = std::time::Instant::now();
    let path_shared: Arc<std::path::PathBuf> = Arc::new(path.to_path_buf());
    let report = |phase: ScanPhase, bytes_done: u64, bytes_total: u64, events: u64| {
        if let Some(progress) = &cb.progress {
            progress(ScanProgress {
                path: path_shared.clone(),
                phase,
                bytes_done,
                bytes_total,
                events,
            });
        }
    };

    // 1. 只读打开 + 身份快照
    let (mut scanner, identity, bom_encoding) = LineScanner::open(path, opts.max_line_bytes)?;
    let file_len = scanner.file_len();
    let mut encoding = opts.encoding_hint.unwrap_or(bom_encoding);

    // 2. 采样识别 Profile（内部会回卷到起点）；顺带用采样修正编码
    report(ScanPhase::Detect, 0, file_len, 0);
    let (profile_id, head_match_ratio, sample) =
        detect::detect_profile(&mut scanner, builtin::builtin_parsers())?;
    if opts.encoding_hint.is_none() {
        encoding = refine_encoding(&sample, encoding);
    }
    let parser_impl = builtin::find_parser(&profile_id);

    // 3. 逐行索引
    report(ScanPhase::Indexing, 0, file_len, 0);
    let time_parser = TimeParser {
        default_tz_offset_min: opts.default_tz_offset_min,
    };
    let mut machine = ParserMachine::new(parser_impl, time_parser, opts.max_event_bytes as u64);

    let mut last_progress_bytes: u64 = 0;
    let mut line_count: u64 = 0;
    let mut last_progress_lines: u64 = 0;
    let mut cancelled = false;

    loop {
        // 取消检查：约每块（1MiB）一次
        if cb.should_cancel() {
            cancelled = true;
            break;
        }
        let Some(line) = scanner.next_line()? else {
            break;
        };
        machine.feed_line(&line);
        line_count += 1;
        if line_count - last_progress_lines >= PROGRESS_LINES_STEP
            || line.end - last_progress_bytes >= PROGRESS_BYTES_STEP
        {
            last_progress_lines = line_count;
            last_progress_bytes = line.end;
            report(
                ScanPhase::Indexing,
                line.end,
                file_len,
                machine.event_count() as u64,
            );
        }
    }

    if cancelled {
        report(ScanPhase::Cancelled, last_progress_bytes, file_len, 0);
        return Err(CoreError::Cancelled);
    }

    // 4. 收尾
    let (entries, mut stats) = machine.finish(file_len);
    stats.total_lines = line_count;
    stats.head_match_ratio = head_match_ratio;
    stats.duration = started.elapsed();
    report(ScanPhase::Done, file_len, file_len, entries.len() as u64);

    let meta = FileMeta {
        file_id,
        path: path.to_path_buf(),
        file_size: identity.len,
        modified_at: identity.mtime,
        encoding,
        profile: Some(profile_id),
    };

    Ok(ScanOutput {
        meta,
        entries,
        stats,
    })
}

/// 由 CLI 参数名构造 `ProfileId`（"auto" 返回 `None` 走自动识别）。
pub fn profile_id_from_str(s: &str) -> Option<ProfileId> {
    match s {
        "auto" => None,
        "springboot" => Some(ProfileId::Builtin(BuiltinProfile::SpringBootDefault)),
        "logback-std" => Some(ProfileId::Builtin(BuiltinProfile::LogbackStd)),
        "logback-nothread" => Some(ProfileId::Builtin(BuiltinProfile::LogbackNoThread)),
        "raw" => Some(ProfileId::Builtin(BuiltinProfile::Raw)),
        _ => None,
    }
}

/// 由 CLI 参数名构造编码提示。
pub fn encoding_from_str(s: &str) -> Option<SourceEncoding> {
    match s {
        "auto" => None,
        "utf8" => Some(SourceEncoding::Utf8),
        "utf8-bom" => Some(SourceEncoding::Utf8Bom),
        "gbk" => Some(SourceEncoding::Gbk),
        _ => None,
    }
}
