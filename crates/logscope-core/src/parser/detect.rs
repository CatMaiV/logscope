//! Profile 采样识别：文件头 + 中部各采 256 个非空行，**按区域独立评分取最优区域**。
//!
//! 不合并两个区域的命中数：拼接/轮转文件中部可能是另一种格式；
//! 而"头部少量噪音 + 后续正常日志"不应因中部续行稀释而回退 Raw。

use crate::model::error::CoreError;
use crate::model::profile::{BuiltinProfile, EventParser, ProfileId};
use crate::scanner::LineScanner;

/// 每个采样区读取的最多非空行数
const SAMPLE_LINES: usize = 256;
/// 编码判定采样缓冲上限（字节）
const SAMPLE_BYTES_CAP: usize = 64 * 1024;
/// 最低命中率阈值（低于则回退 Raw）
const MIN_RATIO: f32 = 0.5;
/// 区域参与投票的最少非空行数（避免小样本误判）
const MIN_REGION_LINES: usize = 2;

/// 采样识别 Profile。
///
/// 返回 `(profile_id, 命中率, 采样字节)`；采样字节供编码修正使用。
/// 识别完成后 scanner 已回卷到数据起点，可直接进入索引循环。
/// 用指定解析器采样头部区域，返回 `(命中率, 采样字节)`（自定义规则用）。
/// 与自动识别的口径一致：明显续行（空白起始 / Caused by / ...）不计入分母。
pub fn sample_ratio(
    scanner: &mut LineScanner,
    parser: &dyn EventParser,
) -> Result<(f32, Vec<u8>), CoreError> {
    scanner.rewind_to_start()?;
    let mut sample = Vec::new();
    let mut hits = 0usize;
    let mut non_empty = 0usize;
    let mut taken = 0usize;
    while taken < SAMPLE_LINES {
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
        taken += 1;
        if sample.len() < SAMPLE_BYTES_CAP {
            let remain = SAMPLE_BYTES_CAP - sample.len();
            sample.extend_from_slice(&line.bytes[..line.bytes.len().min(remain)]);
        }
        if looks_like_continuation(content) {
            continue;
        }
        non_empty += 1;
        if parser.is_head(content) {
            hits += 1;
        }
    }
    scanner.rewind_to_start()?;
    let ratio = if non_empty == 0 {
        0.0
    } else {
        hits as f32 / non_empty as f32
    };
    Ok((ratio, sample))
}

pub fn detect_profile(
    scanner: &mut LineScanner,
    parsers: &'static [Box<dyn EventParser>],
) -> Result<(ProfileId, f32, Vec<u8>), CoreError> {
    let mut sample = Vec::new();

    scanner.rewind_to_start()?;
    let head = sample_region(scanner, parsers, &mut sample)?;

    let mut middle = None;
    if scanner.seek_to_middle_line()? {
        middle = Some(sample_region(scanner, parsers, &mut sample)?);
    }

    scanner.rewind_to_start()?;

    if head.non_empty == 0 && middle.is_none() {
        return Ok((ProfileId::Builtin(BuiltinProfile::Raw), 0.0, sample));
    }

    // 按区域独立评分：取比率最高的区域；头部区域优先（平手保留 head）
    let mut best_region = region_score(&head, parsers.len());
    if let Some(mid) = &middle {
        let mid_score = region_score(mid, parsers.len());
        if mid_score.1 > best_region.1 {
            best_region = mid_score;
        }
    }

    let (best_idx, ratio) = best_region;
    let id = if ratio >= MIN_RATIO {
        parsers[best_idx].id()
    } else {
        ProfileId::Builtin(BuiltinProfile::Raw)
    };
    Ok((id, ratio, sample))
}

/// 单区域采样结果
struct RegionSample {
    hits: Vec<usize>,
    non_empty: usize,
}

/// 区域评分：`(最佳解析器下标, 命中率)`。区域行数不足时不投票 `(Raw 下标, 0.0)`。
fn region_score(r: &RegionSample, parser_count: usize) -> (usize, f32) {
    if r.non_empty < MIN_REGION_LINES {
        return (parser_count - 1, 0.0); // 指向 Raw（注册表末位）
    }
    // 平手取注册表靠前（更具体优先）：严格大于才替换
    let mut best = 0;
    for (i, h) in r.hits.iter().enumerate() {
        if *h > r.hits[best] {
            best = i;
        }
    }
    (best, r.hits[best] as f32 / r.non_empty as f32)
}

/// 采样一个区域（最多 `SAMPLE_LINES` 个非空行；跳过虚拟断行碎片）。
fn sample_region(
    scanner: &mut LineScanner,
    parsers: &'static [Box<dyn EventParser>],
    sample: &mut Vec<u8>,
) -> Result<RegionSample, CoreError> {
    let mut hits = vec![0usize; parsers.len()];
    let mut non_empty = 0usize;
    let mut taken = 0;
    while taken < SAMPLE_LINES {
        let Some(line) = scanner.next_line()? else {
            break;
        };
        if line.truncated {
            // 超长物理行的虚拟断行碎片：跳过（既不计分也不计入分母）
            continue;
        }
        let content = line.content();
        if content.is_empty() {
            continue;
        }
        taken += 1;
        // 编码采样仍包含续行（正文常在续行里）
        if sample.len() < SAMPLE_BYTES_CAP {
            let remain = SAMPLE_BYTES_CAP - sample.len();
            sample.extend_from_slice(&line.bytes[..line.bytes.len().min(remain)]);
        }
        if looks_like_continuation(content) {
            continue; // 续行不计入分母、不计分
        }
        non_empty += 1;
        for (i, p) in parsers.iter().enumerate() {
            // Raw 兜底对任何非空行都"命中"，不参与评分（否则会吞掉所有识别结果）
            if matches!(p.id(), ProfileId::Builtin(BuiltinProfile::Raw)) {
                continue;
            }
            if p.is_head(content) {
                hits[i] += 1;
            }
        }
    }
    Ok(RegionSample { hits, non_empty })
}

/// 明显的续行特征（与具体格式无关）：堆栈帧 / Cause 链 / 省略行。
/// 这些行在 Logback 文件中**预期存在**，不应计入"本可以是头行"的分母——
/// 否则堆栈为主的错误日志转储会把命中率稀释到阈值以下。
fn looks_like_continuation(line: &[u8]) -> bool {
    if line.is_empty() {
        return true;
    }
    if line[0] == b'\t' || line[0] == b' ' {
        return true;
    }
    line.starts_with(b"Caused by:") || line.starts_with(b"Suppressed:") || line.starts_with(b"...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::builtin::builtin_parsers;
    use std::io::Write;

    fn detect(content: &[u8]) -> (ProfileId, f32) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        std::fs::write(&p, content).unwrap();
        let (mut s, _, _) = LineScanner::open(&p, 1024 * 1024).unwrap();
        let (id, ratio, _) = detect_profile(&mut s, builtin_parsers()).unwrap();
        (id, ratio)
    }

    #[test]
    fn detects_springboot() {
        let mut buf = Vec::new();
        for i in 0..50 {
            writeln!(
                buf,
                "2026-11-20 09:15:{:02}.123  INFO 12345 --- [main] com.example.Foo : msg {}",
                i % 60,
                i
            )
            .unwrap();
        }
        let (id, ratio) = detect(&buf);
        assert_eq!(id, ProfileId::Builtin(BuiltinProfile::SpringBootDefault));
        assert!(ratio >= 0.99);
    }

    #[test]
    fn detects_logback_std_with_continuations() {
        let mut buf = Vec::new();
        for i in 0..50 {
            writeln!(
                buf,
                "2026-11-20 09:15:{:02}.123 [main] INFO  com.example.Foo - msg {}",
                i % 60,
                i
            )
            .unwrap();
            writeln!(buf, "  at com.example.Foo.bar(Foo.java:{})", i).unwrap();
        }
        // 续行（空白起始的堆栈帧）不计入分母：比率应为 1.0
        let (id, ratio) = detect(&buf);
        assert_eq!(id, ProfileId::Builtin(BuiltinProfile::LogbackStd));
        assert!(ratio >= 0.99);
    }

    #[test]
    fn head_noise_still_detects() {
        // 头部噪音 + 后续正常日志：头部区域比率 5/7 ≈ 0.71，应识别而非回退
        let mut buf = Vec::new();
        writeln!(buf, "JVM started").unwrap();
        writeln!(buf, "WARNING: Using incubator modules").unwrap();
        for i in 0..5 {
            writeln!(
                buf,
                "2026-11-20 09:15:{:02}.123 [main] INFO  c.Foo - msg {}",
                i, i
            )
            .unwrap();
        }
        let (id, _) = detect(&buf);
        assert_eq!(id, ProfileId::Builtin(BuiltinProfile::LogbackStd));
    }

    #[test]
    fn prose_falls_back_to_raw() {
        let prose = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit.\n\
                      Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\n\
                      Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.\n\
                      Duis aute irure dolor in reprehenderit in voluptate velit esse.\n";
        let (id, ratio) = detect(prose);
        assert_eq!(id, ProfileId::Builtin(BuiltinProfile::Raw));
        assert!(ratio < 0.5);
    }

    #[test]
    fn empty_file_raw() {
        let (id, _) = detect(b"");
        assert_eq!(id, ProfileId::Builtin(BuiltinProfile::Raw));
    }
}
