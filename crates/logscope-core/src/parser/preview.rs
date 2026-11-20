//! 解析预览（M4）：导入前/应用自定义规则前检查命中情况。
//!
//! 不做索引，只读文件头部的若干非空行，逐行报告是否被识别为事件头及解析出的字段。

use crate::model::error::CoreError;
use crate::model::profile::EventParser;
use crate::parser::{builtin, detect, time::TimeParser};
use crate::scanner::LineScanner;
use std::path::Path;

/// 单行预览结果
#[derive(Clone, Debug)]
pub struct PreviewRow {
    /// 行内容（已去 CR，超长截断）
    pub line: String,
    /// 是否被识别为事件头
    pub matched: bool,
    pub timestamp_ms: Option<i64>,
    pub level: Option<String>,
    pub thread: Option<String>,
    pub logger: Option<String>,
}

/// 预览结果
#[derive(Clone, Debug)]
pub struct Preview {
    /// 自动识别到的内置规则名（仅当未提供自定义规则时）
    pub detected: Option<String>,
    /// 识别为事件头的行数
    pub matched: usize,
    /// 参与判断的非空行数
    pub sampled: usize,
    pub rows: Vec<PreviewRow>,
}

/// 单行最大展示字节（防止超长行撑爆 UI）
const MAX_ROW_BYTES: usize = 500;

/// 预览文件解析效果。
///
/// - `pattern = Some(..)`：用自定义规则（校验与扫描时一致）
/// - `pattern = None`：先自动识别内置规则，再用它预览
pub fn preview_file(
    path: &Path,
    pattern: Option<&str>,
    tz_offset_min: i32,
    max_lines: usize,
) -> Result<Preview, CoreError> {
    let (mut scanner, _, _) = LineScanner::open(path, 4 * 1024 * 1024)?;

    let custom = match pattern.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => Some(builtin::build_custom_parser(p)?),
        None => None,
    };

    let (detected, parser): (Option<String>, &dyn EventParser) = match &custom {
        Some(p) => (None, p),
        None => {
            let (id, _ratio, _) = detect::detect_profile(&mut scanner, builtin::builtin_parsers())?;
            let name = id.display_name();
            (Some(name), builtin::find_parser(&id))
        }
    };

    let time = TimeParser {
        default_tz_offset_min: tz_offset_min,
    };
    let max_lines = max_lines.clamp(1, 200);
    let mut rows = Vec::with_capacity(max_lines);
    let mut matched = 0usize;
    let mut sampled = 0usize;

    while sampled < max_lines {
        let Some(line) = scanner.next_line()? else {
            break;
        };
        if line.truncated {
            continue;
        }
        let content = line.content();
        if content.is_empty() {
            continue;
        }
        sampled += 1;
        let is_head = parser.is_head(content);
        let mut row = PreviewRow {
            line: decode_line(&content[..content.len().min(MAX_ROW_BYTES)]),
            matched: is_head,
            timestamp_ms: None,
            level: None,
            thread: None,
            logger: None,
        };
        if is_head {
            matched += 1;
            let head = parser.parse_head(content, &time);
            row.timestamp_ms = head.timestamp_ms;
            row.level = Some(head.level.as_str().to_string());
            row.thread = head
                .thread
                .map(|(s, e)| decode_line(&content[s as usize..e as usize]));
            row.logger = head
                .logger
                .map(|(s, e)| decode_line(&content[s as usize..e as usize]));
        }
        rows.push(row);
    }

    Ok(Preview {
        detected,
        matched,
        sampled,
        rows,
    })
}

fn decode_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGBACK: &str = "2026-11-20 09:00:00.000 [main] INFO  c.e.Foo - hello\n\
                           \tat a.b.c(C.java:1)\n\
                           2026-11-20 09:00:01.000 [worker-1] ERROR c.e.Bar - boom\n";

    fn write(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        std::fs::write(&p, content).unwrap();
        (dir, p)
    }

    #[test]
    fn preview_auto_detects_builtin() {
        let (_d, p) = write(LOGBACK);
        let pv = preview_file(&p, None, 480, 10).unwrap();
        assert!(pv.detected.is_some());
        assert_eq!(pv.sampled, 3);
        assert_eq!(pv.matched, 2);
        assert_eq!(pv.rows[0].level.as_deref(), Some("INFO"));
        assert_eq!(pv.rows[0].thread.as_deref(), Some("main"));
        assert_eq!(pv.rows[0].logger.as_deref(), Some("c.e.Foo"));
        assert!(!pv.rows[1].matched, "堆栈行不应命中");
    }

    #[test]
    fn preview_with_custom_pattern() {
        // 例如只有 "HH:mm:ss 级别 消息" 的简版格式
        let content = "09:00:00 WARN  disk almost full\n09:00:01 INFO  ok\n";
        let (_d, p) = write(content);
        let pattern =
            r"(?-u)^(?P<msg_time>\d{2}:\d{2}:\d{2})\s+(?P<level>TRACE|DEBUG|INFO|WARN|ERROR|FATAL)";
        // 缺少 ts 组 → 应报错
        assert!(preview_file(&p, Some(pattern), 480, 10).is_err());
        // 提供 ts 组 → 命中（时间只有时分秒时，用户需使用含日期格式；此处验证识别机制）
        let ok_pattern =
            r"(?-u)^(?P<ts>\d{2}:\d{2}:\d{2})\s+(?P<level>TRACE|DEBUG|INFO|WARN|ERROR|FATAL)";
        let pv = preview_file(&p, Some(ok_pattern), 480, 10).unwrap();
        assert_eq!(pv.detected, None);
        assert_eq!(pv.matched, 2);
        assert_eq!(pv.rows[0].level.as_deref(), Some("WARN"));
    }
}
