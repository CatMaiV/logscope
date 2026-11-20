//! 基于仓库夹具的集成测试（验收红线：偏移无缝覆盖 / 取消不留半成品 / 多行归并 / 未知时间分组）。
//!
//! CWD 为 crate 根（crates/logscope-core），夹具在 ../../fixtures。

use logscope_core::detail::DetailReader;
use logscope_core::index::{build_workspace, FileIndex};
use logscope_core::model::file_meta::SourceEncoding;
use logscope_core::model::level::LogLevel;
use logscope_core::model::profile::{BuiltinProfile, ProfileId};
use logscope_core::model::stats::{ScanCallbacks, ScanOptions, ScanPhase};
use logscope_core::scanner::scan_file;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn scan_id(path: &Path, file_id: u32) -> logscope_core::scanner::ScanOutput {
    let opts = ScanOptions::default();
    let cb = ScanCallbacks {
        progress: None,
        cancel: Some(Arc::new(AtomicBool::new(false))),
    };
    scan_file(path, &opts, &cb, file_id).expect("scan ok")
}

fn scan(path: &Path) -> logscope_core::scanner::ScanOutput {
    scan_id(path, 0)
}

/// 验收红线：事件区间 [start,end) 从 BOM 后无缝覆盖到文件尾。
fn assert_full_coverage(out: &logscope_core::scanner::ScanOutput) {
    let bom = if out.meta.encoding == SourceEncoding::Utf8Bom {
        3u64
    } else {
        0
    };
    let mut cursor = bom;
    for e in &out.entries {
        assert_eq!(e.start_offset, cursor, "事件 #{} 起点不连续", e.sequence_no);
        assert!(e.end_offset > e.start_offset);
        cursor = e.end_offset;
    }
    assert_eq!(cursor, out.meta.file_size, "事件未覆盖到文件尾");
}

#[test]
fn basic_app_parses() {
    let out = scan(&fixture("basic_app.log"));
    assert_full_coverage(&out);
    assert_eq!(
        out.meta.profile,
        Some(ProfileId::Builtin(BuiltinProfile::LogbackStd))
    );
    assert_eq!(out.stats.unknown_count(), 0, "正常文件不应有 UNKNOWN");
    assert_eq!(out.stats.events, 42);
    assert_eq!(out.stats.error_count(), 2);
    assert!(out.stats.warn_count() >= 4);
    let lo = out.stats.time_min_ms.expect("有时间范围");
    assert_eq!(lo % 1000, 0);
    assert!(out.stats.time_max_ms.unwrap() > lo);
}

#[test]
fn multiline_stack_counts_as_single_event() {
    let out = scan(&fixture("multiline_stacktrace.log"));
    assert_full_coverage(&out);
    // 头：INFO / ERROR(+大堆栈) / INFO / INFO / ERROR(+堆栈)
    assert_eq!(out.stats.events, 5);
    let error_events: Vec<_> = out
        .entries
        .iter()
        .filter(|e| e.level == LogLevel::Error)
        .collect();
    assert_eq!(error_events.len(), 2);
    assert!(error_events[0].byte_len() > 1500, "大堆栈应完整归并");
}

#[test]
fn show_raw_bytes_match_file_exactly() {
    let out = scan(&fixture("multiline_stacktrace.log"));
    let mut reader = DetailReader::open(&out.meta, 480).unwrap();
    let file_bytes = std::fs::read(fixture("multiline_stacktrace.log")).unwrap();
    // 每个事件：read_raw_exact 与文件对应区间逐字节一致（偏移正确性的 diff 级验证）
    for e in &out.entries {
        let raw = reader.read_raw_exact(e).unwrap();
        assert_eq!(
            &file_bytes[e.start_offset as usize..e.end_offset as usize],
            raw.as_slice(),
            "事件 #{} 偏移不一致",
            e.sequence_no
        );
    }
}

#[test]
fn crlf_offsets_include_cr() {
    let out = scan(&fixture("crlf_windows.log"));
    assert_full_coverage(&out);
    assert_eq!(out.stats.events, 4);
    let mut reader = DetailReader::open(&out.meta, 480).unwrap();
    let text = reader.read_text(&out.entries[0]).unwrap();
    assert!(text.contains("Windows CRLF 服务启动"));
    assert!(!text.ends_with('\r'), "read_text 应去除尾部 CR");
    // 堆栈续行归并到第 3 个事件（ERROR）
    assert!(out.entries[2].byte_len() > 100);
    assert_eq!(out.entries[2].level, LogLevel::Error);
}

#[test]
fn utf8_bom_first_event_starts_at_3() {
    let out = scan(&fixture("utf8_bom.log"));
    assert_eq!(out.meta.encoding, SourceEncoding::Utf8Bom);
    assert_eq!(out.entries[0].start_offset, 3);
    assert_full_coverage(&out);
}

#[test]
fn chinese_offsets_align() {
    let out = scan(&fixture("chinese_message.log"));
    assert_full_coverage(&out);
    let mut reader = DetailReader::open(&out.meta, 480).unwrap();
    let file_bytes = std::fs::read(fixture("chinese_message.log")).unwrap();
    for e in &out.entries {
        let raw = reader.read_raw_exact(e).unwrap();
        assert_eq!(
            &file_bytes[e.start_offset as usize..e.end_offset as usize],
            raw.as_slice()
        );
    }
    let text = reader.read_text(&out.entries[5]).unwrap();
    assert!(text.contains("支付失败"), "第 6 条事件应含中文消息");
}

#[test]
fn gbk_fixture_decodes() {
    let path = fixture("gbk_encoded.log");
    if !path.exists() {
        eprintln!("skip: gbk_encoded.log 未生成（scripts/gen-gbk-fixture.ps1）");
        return;
    }
    let out = scan(&path);
    assert_eq!(out.meta.encoding, SourceEncoding::Gbk);
    assert_full_coverage(&out);
    let mut reader = DetailReader::open(&out.meta, 480).unwrap();
    let text = reader.read_text(&out.entries[0]).unwrap();
    assert!(text.contains("应用开始初始化"), "GBK 解码失败: {}", text);
}

#[test]
fn bad_timestamps_kept_with_level() {
    let out = scan(&fixture("bad_timestamps.log"));
    assert_full_coverage(&out);
    // 6 个头（含 3 个坏时间）+ 1 个裸续行并入前一事件 = 6 个事件
    assert_eq!(out.stats.events, 6);
    let bad: Vec<_> = out
        .entries
        .iter()
        .filter(|e| e.flags.has_bad_timestamp())
        .collect();
    assert_eq!(bad.len(), 3, "2026-13-45 / 25:61:61 / 2027-02-29");
    for e in &bad {
        assert!(e.timestamp_ms.is_none());
        assert_ne!(e.level, LogLevel::Unknown, "坏时间但结构匹配应保留级别");
    }
    assert_eq!(out.stats.no_timestamp_events, 3);
    // 裸续行并入 09:00:05 的事件（第 5 个，idx=4）
    let recovered = &out.entries[4];
    assert!(recovered.byte_len() > 60, "裸续行应并入前一事件");
    assert!(!recovered.flags.is_unknown());
}

#[test]
fn unordered_timestamps_global_sort() {
    let out = scan(&fixture("unordered_timestamps.log"));
    let idx = Arc::new(FileIndex::new(
        out.meta.clone(),
        out.entries.clone(),
        out.stats.clone(),
    ));
    let ws = build_workspace(vec![idx]);
    let stamps: Vec<i64> = ws
        .time_order
        .iter()
        .filter_map(|r| ws.get(*r).and_then(|e| e.timestamp_ms))
        .collect();
    let mut sorted = stamps.clone();
    sorted.sort();
    assert_eq!(stamps, sorted, "全局视图必须按时间排序");
    assert_eq!(stamps.len(), 6);
    // 同毫秒 10s（=stamps 最大值）出现两次：物理序稳定（seq 0 与 4）
    let max_ts = *stamps.last().unwrap();
    let tens: Vec<u32> = ws
        .time_order
        .iter()
        .filter(|r| ws.get(**r).and_then(|e| e.timestamp_ms) == Some(max_ts))
        .map(|r| r.sequence_no)
        .collect();
    assert_eq!(tens, vec![0, 4], "同毫秒按文件内序号稳定排序");
}

#[test]
fn no_trailing_newline_last_event() {
    let out = scan(&fixture("no_trailing_newline.log"));
    assert_full_coverage(&out);
    let last = out.entries.last().unwrap();
    assert_eq!(last.end_offset, out.meta.file_size);
    assert_eq!(last.level, LogLevel::Warn);
}

#[test]
fn empty_file_zero_events_no_crash() {
    let out = scan(&fixture("empty.log"));
    assert!(out.entries.is_empty());
    assert_eq!(out.stats.events, 0);
}

#[test]
fn non_logback_falls_back_to_raw() {
    let out = scan(&fixture("non_logback.txt"));
    assert_eq!(
        out.meta.profile,
        Some(ProfileId::Builtin(BuiltinProfile::Raw))
    );
    assert_full_coverage(&out);
    assert_eq!(out.stats.unknown_count(), out.stats.events as u64);
    assert_eq!(out.stats.no_timestamp_events, out.stats.events);
}

#[test]
fn iso8601_tz_offsets_respected() {
    let out = scan(&fixture("iso8601_tz.log"));
    assert_eq!(out.stats.events, 6);
    let v: Vec<i64> = out.entries.iter().filter_map(|e| e.timestamp_ms).collect();
    assert_eq!(v.len(), 6);
    // [0] 09:00:00.000+08:00 → UTC 01:00:00.000
    // [1] 09:00:01.000Z     → UTC 09:00:01.000 ：差 8h + 1s
    assert_eq!(v[1] - v[0], 8 * 3600 * 1000 + 1000);
    // [2] 09:00:02.123+0800 与 [0] 差 2.123s
    assert_eq!(v[2] - v[0], 2_123);
    // [3] 09:00:03.500-05:00 → UTC 14:00:03.500 ：与 [0] 差 13h + 3.5s
    assert_eq!(v[3] - v[0], 13 * 3600 * 1000 + 3_500);
    // [4] 09:00:04.000 无时区 → 默认 +480 → UTC 01:00:04.000 ：与 [0] 差 4s
    assert_eq!(v[4] - v[0], 4_000);
}

#[test]
fn springboot_default_detected() {
    let out = scan(&fixture("springboot_default.log"));
    assert_eq!(
        out.meta.profile,
        Some(ProfileId::Builtin(BuiltinProfile::SpringBootDefault))
    );
    assert_full_coverage(&out);
    assert_eq!(out.stats.events, 7);
    assert_eq!(out.stats.error_count(), 1);
}

#[test]
fn multifile_same_ms_stable() {
    let a = scan_id(&fixture("multifile/a.log"), 0);
    let b = scan_id(&fixture("multifile/b.log"), 1);
    let ia = Arc::new(FileIndex::new(
        a.meta.clone(),
        a.entries.clone(),
        a.stats.clone(),
    ));
    let ib = Arc::new(FileIndex::new(
        b.meta.clone(),
        b.entries.clone(),
        b.stats.clone(),
    ));
    let ws = build_workspace(vec![ia, ib]);
    let order: Vec<String> = ws
        .time_order
        .iter()
        .filter_map(|r| {
            let _e = ws.get(*r)?;
            Some(format!(
                "{}#{}",
                if r.file_id == 0 { "a" } else { "b" },
                r.sequence_no
            ))
        })
        .collect();
    assert_eq!(
        order,
        vec![
            "a#0", "b#0", // t=0 同毫秒：a 物理序在前
            "a#1", "a#2", "b#1", // t=1s
            "b#2", // t=3s
            "a#3", "a#4", "b#3", // t=5s
            "b#4", // t=7s
            "a#5", // t=9s
        ],
        "跨文件同毫秒稳定排序"
    );
}

#[test]
fn cancel_mid_scan_returns_cancelled() {
    let path = fixture("huge_event.log");
    if !path.exists() {
        eprintln!("skip: huge_event.log 未生成（node scripts/prepare-fixtures.mjs）");
        return;
    }
    let opts = ScanOptions::default();
    let flag = Arc::new(AtomicBool::new(false));
    let cb = ScanCallbacks {
        progress: Some({
            let flag = flag.clone();
            Arc::new(move |p| {
                if p.phase == ScanPhase::Indexing && p.bytes_done > 1024 * 1024 {
                    flag.store(true, Ordering::Release);
                }
            })
        }),
        cancel: Some(flag),
    };
    let result = scan_file(&path, &opts, &cb, 0);
    assert!(matches!(result, Err(logscope_core::CoreError::Cancelled)));
}

#[test]
fn huge_event_truncation_invariant() {
    let path = fixture("huge_event.log");
    if !path.exists() {
        eprintln!("skip: huge_event.log 未生成");
        return;
    }
    let opts = ScanOptions {
        max_event_bytes: 1024 * 1024, // 1MB 上限
        ..Default::default()
    };
    let cb = ScanCallbacks::default();
    let out = scan_file(&path, &opts, &cb, 0).unwrap();
    assert_full_coverage(&out);
    assert!(out.stats.truncated_events >= 1);
    assert!(
        out.stats.unknown_count() >= 1,
        "截断后的续行应进 UNKNOWN，不丢弃"
    );
    for e in &out.entries {
        assert!(e.byte_len() <= 1024 * 1024 + 4 * 1024 * 1024);
    }
}

#[test]
fn long_line_virtual_break_invariant() {
    let path = fixture("long_line.log");
    if !path.exists() {
        eprintln!("skip: long_line.log 未生成");
        return;
    }
    // 默认 8MiB 事件上限：头行 + 5MB 超长行 = 1 个事件，尾行 = 1 个事件
    let out = scan(&path);
    assert_full_coverage(&out);
    assert_eq!(out.stats.events, 2);
    assert!(
        out.entries[0].byte_len() > 5 * 1024 * 1024,
        "超长行应归并入前一事件"
    );
    assert_eq!(out.entries[0].level, LogLevel::Info);

    // 显式收紧事件上限：超长事件被截断为多段，偏移不变量仍成立
    let opts = ScanOptions {
        max_event_bytes: 1024 * 1024,
        ..Default::default()
    };
    let cb = ScanCallbacks::default();
    let out2 = scan_file(&path, &opts, &cb, 0).unwrap();
    assert_full_coverage(&out2);
    assert!(out2.stats.events > 2, "截断后应产生更多事件段");
    assert!(out2.stats.truncated_events >= 1);
}
