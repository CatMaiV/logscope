//! logscope-cli — 核心能力的命令行验证（开发计划 §10 启动顺序）。
//!
//! 退出码：0 成功 | 2 文件不可读 | 3 零事件 | 4 取消 | 1 其他错误

use anyhow::{bail, Context as _, Result};
use clap::{Parser, Subcommand};
use logscope_core::detail::DetailReader;
use logscope_core::index::{build_workspace, FileIndex, WorkspaceIndex};
use logscope_core::model::level::{LevelSet, LogLevel};
use logscope_core::model::stats::{ScanCallbacks, ScanOptions};
use logscope_core::scanner::{encoding_from_str, profile_id_from_str, scan_file};
use logscope_core::search::{SearchQuery, SingleSource, TextMatcher};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

const EXIT_IO: i32 = 2;
const EXIT_EMPTY: i32 = 3;
/// 预留给 M2 的可取消长查询（当前 CLI 无交互式取消路径）
#[allow(dead_code)]
const EXIT_CANCELLED: i32 = 4;

#[derive(Parser)]
#[command(name = "logscope-cli", about = "LogScope 核心能力验证 CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 解析文件并输出统计
    Parse {
        file: PathBuf,
        #[arg(long, default_value = "auto")]
        profile: String,
        #[arg(long, default_value = "auto")]
        encoding: String,
        #[arg(long, default_value_t = 8 * 1024 * 1024)]
        max_event_bytes: usize,
        #[arg(long)]
        json: bool,
        /// 自定义解析正则（需含 ts / level 命名组）
        #[arg(long)]
        custom_pattern: Option<String>,
    },
    /// 按事件 ID 读取完整原文（含多行堆栈）
    Show {
        file: PathBuf,
        #[arg(long)]
        id: u32,
        /// 输出未解码的原始字节（用于与源文件 diff 验证偏移）
        #[arg(long)]
        raw: bool,
    },
    /// 同文件物理顺序上下文
    Context {
        file: PathBuf,
        #[arg(long)]
        id: u32,
        #[arg(long, default_value_t = 3)]
        before: u32,
        #[arg(long, default_value_t = 3)]
        after: u32,
        /// 只看同线程的上下文（无线程信息时回退物理顺序）
        #[arg(long)]
        thread: bool,
    },
    /// 按级别 / 时间 / 关键词过滤
    Filter {
        file: PathBuf,
        #[arg(long)]
        level: Option<String>,
        /// 起始时间 "YYYY-MM-DD HH:MM:SS[.fff]"（默认时区）或 epoch 毫秒
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        text_ci: bool,
        /// 将 --text 按正则解释（Rust regex 语法，不支持前后向断言）
        #[arg(long)]
        regex: bool,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// 时间桶统计
    Timeline {
        file: PathBuf,
        #[arg(long, default_value = "auto")]
        bucket: String,
        #[arg(long)]
        json: bool,
    },
    /// 生成合成基准日志
    Gen {
        out: PathBuf,
        /// 目标大小，如 100MB / 1GB / 500KB（也接受纯字节数）
        #[arg(long)]
        size: String,
        #[arg(long, default_value_t = 0.02)]
        err_rate: f64,
        #[arg(long, default_value_t = 0.6)]
        stack_rate: f64,
        #[arg(long, default_value_t = 42)]
        seed: u64,
    },
    /// 解析预览：查看自动识别/自定义规则对文件头部的命中情况
    Preview {
        file: PathBuf,
        /// 自定义正则（需含 ts / level 命名组）
        #[arg(long)]
        pattern: Option<String>,
        #[arg(long, default_value_t = 40)]
        lines: usize,
    },
    /// 基准测试：解析 / 级别过滤 / 文本过滤 / 时间轴 各阶段耗时（多次取中位数）
    Bench {
        file: PathBuf,
        /// 文本过滤关键词（默认 Exception）
        #[arg(long, default_value = "Exception")]
        text: String,
        #[arg(long, default_value_t = 3)]
        runs: usize,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli.cmd) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("错误: {:#}", e);
            if is_io_error(&e) {
                EXIT_IO
            } else {
                1
            }
        }
    };
    std::process::exit(code);
}

fn is_io_error(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|c| c.downcast_ref::<std::io::Error>().is_some())
}

fn run(cmd: Cmd) -> Result<i32> {
    match cmd {
        Cmd::Parse {
            file,
            profile,
            encoding,
            max_event_bytes,
            json,
            custom_pattern,
        } => cmd_parse(
            &file,
            &profile,
            &encoding,
            max_event_bytes,
            json,
            custom_pattern.as_deref(),
        ),
        Cmd::Show { file, id, raw } => cmd_show(&file, id, raw),
        Cmd::Context {
            file,
            id,
            before,
            after,
            thread,
        } => cmd_context(&file, id, before, after, thread),
        Cmd::Filter {
            file,
            level,
            from,
            to,
            text,
            text_ci,
            regex,
            limit,
        } => cmd_filter(&file, level, from, to, text, text_ci, regex, limit),
        Cmd::Timeline { file, bucket, json } => cmd_timeline(&file, &bucket, json),
        Cmd::Gen {
            out,
            size,
            err_rate,
            stack_rate,
            seed,
        } => cmd_gen(&out, &size, err_rate, stack_rate, seed),
        Cmd::Preview {
            file,
            pattern,
            lines,
        } => cmd_preview(&file, pattern.as_deref(), lines),
        Cmd::Bench { file, text, runs } => cmd_bench(&file, &text, runs),
    }
}

/// 扫描 + 建索引的公共路径
fn scan_and_index(
    path: &std::path::Path,
    profile: &str,
    encoding: &str,
    custom_pattern: Option<&str>,
) -> Result<(Arc<FileIndex>, DetailReader, WorkspaceIndex, ScanOptions)> {
    // "auto" → None（自动识别）；其余名字必须有效
    let profile_hint = profile_id_from_str(profile);
    if profile != "auto" && profile_hint.is_none() {
        bail!(
            "未知 profile: {}（可选 auto|springboot|logback-std|logback-nothread|raw）",
            profile
        );
    }
    let encoding_hint = encoding_from_str(encoding);
    if encoding != "auto" && encoding_hint.is_none() {
        bail!("未知 encoding: {}（可选 auto|utf8|utf8-bom|gbk）", encoding);
    }
    let opts = ScanOptions {
        profile_hint,
        encoding_hint,
        custom_pattern: custom_pattern.map(str::to_string),
        ..Default::default()
    };
    let cb = ScanCallbacks::default();
    let out =
        scan_file(path, &opts, &cb, 0).with_context(|| format!("扫描失败: {}", path.display()))?;
    let idx = Arc::new(FileIndex::new(
        out.meta.clone(),
        out.entries,
        out.stats.clone(),
    ));
    let reader = DetailReader::open(&out.meta, opts.default_tz_offset_min)?;
    let ws = build_workspace(vec![idx.clone()]);
    Ok((idx, reader, ws, opts))
}

fn fmt_ts(ms: i64, tz_min: i32) -> String {
    let offset = chrono::FixedOffset::east_opt(tz_min * 60).unwrap();
    match chrono::DateTime::from_timestamp_millis(ms) {
        Some(dt) => dt
            .with_timezone(&offset)
            .format("%Y-%m-%d %H:%M:%S%.3f")
            .to_string(),
        None => ms.to_string(),
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ParseJson {
    file: String,
    encoding: String,
    profile: String,
    #[serde(flatten)]
    stats: logscope_core::model::stats::ParseStats,
    time_min: Option<String>,
    time_max: Option<String>,
}

fn cmd_parse(
    file: &std::path::Path,
    profile: &str,
    encoding: &str,
    _max_event_bytes: usize,
    json: bool,
    custom_pattern: Option<&str>,
) -> Result<i32> {
    let (idx, _reader, _ws, opts) = scan_and_index(file, profile, encoding, custom_pattern)?;
    let stats = &idx.stats;
    let tz = opts.default_tz_offset_min;

    if json {
        let payload = ParseJson {
            file: file.display().to_string(),
            encoding: idx.meta.encoding.as_str().to_string(),
            profile: idx
                .meta
                .profile
                .map(|p| p.display_name())
                .unwrap_or_default(),
            stats: stats.clone(),
            time_min: stats.time_min_ms.map(|m| fmt_ts(m, tz)),
            time_max: stats.time_max_ms.map(|m| fmt_ts(m, tz)),
        };
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(0);
    }

    println!("文件: {} ({} 字节)", file.display(), idx.meta.file_size);
    println!(
        "Profile: {}（采样命中率 {:.0}%）",
        idx.meta
            .profile
            .map(|p| p.display_name())
            .unwrap_or_else(|| "-".into()),
        stats.head_match_ratio * 100.0
    );
    println!("编码: {}", idx.meta.encoding.as_str());
    println!("事件: {}", stats.events);
    println!(
        "  TRACE {} | DEBUG {} | INFO {} | WARN {} | ERROR {} | FATAL {} | UNKNOWN {}",
        stats.level_counts[0],
        stats.level_counts[1],
        stats.level_counts[2],
        stats.level_counts[3],
        stats.level_counts[4],
        stats.level_counts[5],
        stats.level_counts[6]
    );
    match (stats.time_min_ms, stats.time_max_ms) {
        (Some(lo), Some(hi)) => println!("时间范围: {} — {}", fmt_ts(lo, tz), fmt_ts(hi, tz)),
        _ => println!("时间范围: （无有效时间戳）"),
    }
    println!(
        "解析失败事件(UNKNOWN): {} | 无时间戳: {} | 截断: {}",
        stats.unknown_count(),
        stats.no_timestamp_events,
        stats.truncated_events
    );
    println!(
        "总行数: {} | 耗时: {:.0}ms",
        stats.total_lines,
        stats.duration.as_secs_f64() * 1000.0
    );

    if stats.events == 0 {
        Ok(EXIT_EMPTY)
    } else {
        Ok(0)
    }
}

fn cmd_show(file: &std::path::Path, id: u32, raw: bool) -> Result<i32> {
    let (idx, mut reader, _ws, _) = scan_and_index(file, "auto", "auto", None)?;
    let Some(entry) = idx.entries.get(id as usize) else {
        bail!(
            "事件 ID {} 不存在（共 {} 个事件，ID 范围 0..{}）",
            id,
            idx.entries.len(),
            idx.entries.len().saturating_sub(1)
        );
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if raw {
        let bytes = reader.read_raw_exact(entry)?;
        out.write_all(&bytes)?;
    } else {
        let text = reader.read_text(entry)?;
        writeln!(out, "{}", text)?;
    }
    Ok(0)
}

fn cmd_context(
    file: &std::path::Path,
    id: u32,
    before: u32,
    after: u32,
    same_thread: bool,
) -> Result<i32> {
    let (idx, mut reader, _ws, opts) = scan_and_index(file, "auto", "auto", None)?;
    let ctx = if same_thread {
        reader.read_context_thread(&idx, id, before, after)
    } else {
        reader.read_context(&idx, id, before, after)
    }
    .with_context(|| format!("读取上下文失败（id={}）", id))?;
    if ctx.is_empty() {
        bail!("事件 ID {} 不存在", id);
    }
    for (e, text) in &ctx {
        let ts = e
            .timestamp_ms
            .map(|m| fmt_ts(m, opts.default_tz_offset_min))
            .unwrap_or_else(|| "??".into());
        let mark = if e.sequence_no == id { '>' } else { ' ' };
        println!(
            "{}#{:<6} {} {:<7} (+{}B)",
            mark,
            e.sequence_no,
            ts,
            e.level.as_str(),
            e.byte_len()
        );
        // 首行摘要
        let first = text.lines().next().unwrap_or("");
        println!("    {}", first);
        let extra = text.lines().count().saturating_sub(1);
        if extra > 0 {
            println!("    └─ +{} 行续行", extra);
        }
    }
    Ok(0)
}

fn parse_time_arg(s: &str, tz_min: i32) -> Result<i64> {
    if s.chars().all(|c| c.is_ascii_digit()) {
        return Ok(s.parse::<i64>()?);
    }
    let naive = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
        .with_context(|| format!("时间格式应为 'YYYY-MM-DD HH:MM:SS[.fff]' 或毫秒数: {}", s))?;
    let utc = naive.and_utc().timestamp_millis();
    Ok(utc - tz_min as i64 * 60_000)
}

fn parse_level_arg(s: &str) -> Result<LevelSet> {
    let mut set = LevelSet::empty();
    for part in s.split(',') {
        let level = match part.trim().to_ascii_uppercase().as_str() {
            "TRACE" => LogLevel::Trace,
            "DEBUG" => LogLevel::Debug,
            "INFO" => LogLevel::Info,
            "WARN" | "WARNING" => LogLevel::Warn,
            "ERROR" => LogLevel::Error,
            "FATAL" => LogLevel::Fatal,
            other => bail!(
                "未知级别: {}（可用 trace|debug|info|warn|error|fatal）",
                other
            ),
        };
        set.insert(level);
    }
    Ok(set)
}

#[allow(clippy::too_many_arguments)]
fn cmd_filter(
    file: &std::path::Path,
    level: Option<String>,
    from: Option<String>,
    to: Option<String>,
    text: Option<String>,
    text_ci: bool,
    regex: bool,
    limit: usize,
) -> Result<i32> {
    let (idx, mut reader, ws, opts) = scan_and_index(file, "auto", "auto", None)?;
    if idx.entries.is_empty() {
        return Ok(EXIT_EMPTY);
    }
    let tz = opts.default_tz_offset_min;
    let query = SearchQuery {
        files: None,
        time_range: match (&from, &to) {
            (Some(f), Some(t)) => Some((parse_time_arg(f, tz)?, parse_time_arg(t, tz)?)),
            (Some(f), None) => Some((parse_time_arg(f, tz)?, i64::MAX)),
            (None, Some(t)) => Some((i64::MIN, parse_time_arg(t, tz)?)),
            (None, None) => None,
        },
        levels: level.as_deref().map(parse_level_arg).transpose()?,
        text: match text {
            Some(t) if regex => Some(TextMatcher::Regex(
                regex::bytes::Regex::new(&t)
                    .with_context(|| format!("正则错误: {}（不支持前后向断言/反向引用）", t))?,
            )),
            Some(t) => Some(TextMatcher::Substring {
                needle: t.into_bytes(),
                case_insensitive: text_ci,
            }),
            None => None,
        },
    };
    let cancel = AtomicBool::new(false);
    let mut provider = SingleSource {
        file_id: 0,
        source: &mut reader,
    };
    let outcome =
        logscope_core::search::execute(&query, &ws, &mut provider, &cancel).context("检索失败")?;

    println!(
        "命中 {}/{} 个事件（扫描原文 {} 字节）",
        outcome.refs.len(),
        ws.total_events(),
        outcome.scanned_bytes
    );
    let shown = outcome.refs.len().min(limit);
    for r in outcome.refs.iter().take(shown) {
        let Some(e) = ws.get(*r) else { continue };
        let ts = e
            .timestamp_ms
            .map(|m| fmt_ts(m, tz))
            .unwrap_or_else(|| "??".into());
        let first_line = {
            let text = reader.read_text(e).unwrap_or_default();
            text.lines().next().unwrap_or("").to_string()
        };
        println!(
            "#{:<6} {} {:<7} {}",
            r.sequence_no,
            ts,
            e.level.as_str(),
            first_line
        );
    }
    if outcome.refs.len() > shown {
        println!(
            "...（其余 {} 条省略，--limit 调整）",
            outcome.refs.len() - shown
        );
    }
    Ok(0)
}

fn cmd_timeline(file: &std::path::Path, bucket: &str, json: bool) -> Result<i32> {
    let (idx, _reader, ws, _opts) = scan_and_index(file, "auto", "auto", None)?;
    if idx.entries.is_empty() {
        return Ok(EXIT_EMPTY);
    }
    let Some((lo, hi)) = ws.time_range() else {
        println!("（无有效时间戳，无法生成时间轴）");
        return Ok(0);
    };
    let spec = match bucket {
        "auto" => logscope_core::timeline::spec_for_range(lo, hi),
        "1s" => mk_spec(lo, hi, 1_000),
        "10s" => mk_spec(lo, hi, 10_000),
        "1m" => mk_spec(lo, hi, 60_000),
        "10m" => mk_spec(lo, hi, 600_000),
        "1h" => mk_spec(lo, hi, 3_600_000),
        "1d" => mk_spec(lo, hi, 86_400_000),
        other => bail!("未知桶宽: {}（可选 auto|1s|10s|1m|10m|1h|1d）", other),
    };
    let buckets = logscope_core::timeline::aggregate(idx.entries.iter(), &spec);

    if json {
        #[derive(serde::Serialize)]
        struct BucketDto {
            start_ms: i64,
            bucket_ms: i64,
            total: u32,
            warn: u32,
            error: u32,
        }
        let dtos: Vec<BucketDto> = buckets
            .iter()
            .enumerate()
            .map(|(i, b)| BucketDto {
                start_ms: spec.start_ms + i as i64 * spec.bucket_ms,
                bucket_ms: spec.bucket_ms,
                total: b.total(),
                warn: b.warn(),
                error: b.error(),
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&dtos)?);
        return Ok(0);
    }

    println!("桶宽: {}ms × {} 桶", spec.bucket_ms, spec.count);
    let max_total: u32 = buckets.iter().map(|b| b.total()).max().unwrap_or(1).max(1);
    for (i, b) in buckets.iter().enumerate() {
        if b.total() == 0 && b.error() == 0 {
            continue;
        }
        let bar_len = ((b.total() as f64 / max_total as f64) * 40.0).ceil() as usize;
        let bar: String = "█".repeat(bar_len);
        println!(
            "{}  total={:<6} warn={:<5} error={:<5} {}",
            fmt_ts(spec.start_ms + i as i64 * spec.bucket_ms, 480),
            b.total(),
            b.warn(),
            b.error(),
            bar
        );
    }
    Ok(0)
}

fn mk_spec(lo: i64, hi: i64, bucket_ms: i64) -> logscope_core::timeline::BucketSpec {
    let span = (hi - lo).max(1);
    let count = (span / bucket_ms + 1).min(logscope_core::timeline::MAX_BUCKETS as i64) as usize;
    logscope_core::timeline::BucketSpec {
        start_ms: lo,
        bucket_ms,
        count: count.max(1),
    }
}

// ---------------- gen ----------------

/// 简易可复现随机源（xorshift64）
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, rate: f64) -> bool {
        (self.next() % 10_000) as f64 / 10_000.0 < rate
    }
}

fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim().to_ascii_uppercase();
    let (num, mult) = if let Some(n) = s.strip_suffix("GB") {
        (n, 1_000_000_000u64)
    } else if let Some(n) = s.strip_suffix("MB") {
        (n, 1_000_000)
    } else if let Some(n) = s.strip_suffix("KB") {
        (n, 1_000)
    } else {
        (s.as_str(), 1)
    };
    let n: f64 = num
        .trim()
        .parse()
        .with_context(|| format!("无法解析大小: {}", s))?;
    Ok((n * mult as f64) as u64)
}

fn cmd_gen(
    out: &std::path::Path,
    size: &str,
    err_rate: f64,
    stack_rate: f64,
    seed: u64,
) -> Result<i32> {
    let target = parse_size(size)?;
    let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    let file =
        std::fs::File::create(out).with_context(|| format!("创建失败: {}", out.display()))?;
    let mut w = std::io::BufWriter::new(file);

    let loggers = [
        "com.example.order.OrderService",
        "com.example.user.AuthController",
        "com.example.pay.PaymentGateway",
        "org.springframework.boot.StartupInfoLogger",
        "org.hibernate.SQL",
        "io.netty.channel.Pipeline",
    ];
    let threads = [
        "main",
        "http-nio-8080-exec-1",
        "http-nio-8080-exec-2",
        "scheduler-1",
        "kafka-consumer-3",
    ];
    let messages = [
        "request handled",
        "cache miss, loading from db",
        "用户登录成功",
        "订单状态更新为已支付",
        "connection pool stats: active=3 idle=12",
        "retrying upstream call, attempt {}",
        "feature flag {} evaluated to true",
    ];

    let mut written: u64 = 0;
    let mut clock_ms: i64 = 1_763_600_000_000; // 2025-11-20 前后的合理起点
    let mut n_events = 0u64;

    while written < target {
        let level = if rng.chance(err_rate) {
            "ERROR"
        } else if rng.chance(0.08) {
            "WARN "
        } else if rng.chance(0.15) {
            "DEBUG"
        } else {
            "INFO "
        };
        let logger = loggers[rng.below(loggers.len() as u64) as usize];
        let thread = threads[rng.below(threads.len() as u64) as usize];
        let msg = messages[rng.below(messages.len() as u64) as usize]
            .replace("{}", &format!("{}", rng.below(1000)));
        clock_ms += rng.below(50) as i64 + 1;

        let ts = {
            let dt = chrono::DateTime::from_timestamp_millis(clock_ms).unwrap();
            dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
        };
        let line = format!("{} [{}] {} {} - {}\n", ts, thread, level, logger, msg);
        w.write_all(line.as_bytes())?;
        written += line.len() as u64;
        n_events += 1;

        // 堆栈
        if level == "ERROR" && rng.chance(stack_rate.clamp(0.0, 1.0)) {
            let depth = 5 + rng.below(20);
            let stack = format!(
                "java.sql.SQLException: ORA-00600: internal error code, arguments: [{}]\n",
                rng.below(99999)
            );
            w.write_all(stack.as_bytes())?;
            written += stack.len() as u64;
            for i in 0..depth {
                let at = format!(
                    "\tat com.example.service.Layer{}{}.method{}(Layer{}{}.java:{})\n",
                    rng.below(4),
                    rng.below(3),
                    rng.below(50),
                    rng.below(4),
                    rng.below(3),
                    10 + rng.below(400)
                );
                w.write_all(at.as_bytes())?;
                written += at.len() as u64;
                let _ = i;
            }
            let caused = "Caused by: java.net.SocketTimeoutException: read timed out\n\t... 3 common frames omitted\n";
            w.write_all(caused.as_bytes())?;
            written += caused.len() as u64;
        }
    }
    w.flush()?;
    drop(w);
    println!(
        "已生成 {} ({} 字节, {} 个事件, seed={})",
        out.display(),
        written,
        n_events,
        seed
    );
    Ok(0)
}

// ---------------- bench ----------------

/// 基准测试：各阶段耗时取多轮中位数（开发计划 §6 M2：100MB / 1GB 基准）。
/// 用法示例：
///   cargo run --release -p logscope-cli -- gen target/bench_100m.log --size 100MB
///   cargo run --release -p logscope-cli -- bench target/bench_100m.log
fn cmd_bench(file: &std::path::Path, text: &str, runs: usize) -> Result<i32> {
    if !file.exists() {
        bail!("文件不存在: {}（可先用 gen 子命令生成）", file.display());
    }
    let runs = runs.clamp(1, 20);
    println!(
        "基准目标: {} ({:.1} MB, {} 轮取中位数)",
        file.display(),
        file.metadata()?.len() as f64 / 1e6,
        runs
    );
    println!("{:<24} {:>10} {:>12}", "阶段", "中位耗时", "产出");

    let mut parse_ms = Vec::new();
    let mut events = 0usize;
    for _ in 0..runs {
        let t = std::time::Instant::now();
        let cb = ScanCallbacks::default();
        let out = scan_file(file, &ScanOptions::default(), &cb, 0)?;
        events = out.stats.events;
        parse_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "{:<24} {:>9.0}ms {:>12}",
        "scan+index（解析）",
        median(&mut parse_ms),
        format!("{} 事件", events)
    );

    // 供后续阶段复用的工作区
    let cb = ScanCallbacks::default();
    let out = scan_file(file, &ScanOptions::default(), &cb, 0)?;
    let idx = Arc::new(FileIndex::new(
        out.meta.clone(),
        out.entries,
        out.stats.clone(),
    ));
    let ws = build_workspace(vec![idx.clone()]);
    let mut reader = DetailReader::open(&out.meta, 480)?;
    let cancel = AtomicBool::new(false);

    // 级别过滤（纯索引，零 IO）
    let mut level_ms = Vec::new();
    let mut level_hits = 0usize;
    for _ in 0..runs {
        let t = std::time::Instant::now();
        let q = SearchQuery {
            levels: Some({
                let mut s = LevelSet::empty();
                s.insert(LogLevel::Error);
                s.insert(LogLevel::Fatal);
                s
            }),
            ..Default::default()
        };
        let mut provider = SingleSource {
            file_id: 0,
            source: &mut reader,
        };
        let r = logscope_core::search::execute(&q, &ws, &mut provider, &cancel)?;
        level_hits = r.refs.len();
        level_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "{:<24} {:>9.0}ms {:>12}",
        "filter: level=ERROR",
        median(&mut level_ms),
        format!("{} 命中", level_hits)
    );

    // 文本过滤（全文扫描）
    let mut text_ms = Vec::new();
    let mut text_hits = 0usize;
    for _ in 0..runs {
        let t = std::time::Instant::now();
        let q = SearchQuery {
            text: Some(TextMatcher::Substring {
                needle: text.as_bytes().to_vec(),
                case_insensitive: false,
            }),
            ..Default::default()
        };
        let mut provider = SingleSource {
            file_id: 0,
            source: &mut reader,
        };
        let r = logscope_core::search::execute(&q, &ws, &mut provider, &cancel)?;
        text_hits = r.refs.len();
        text_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "{:<24} {:>9.0}ms {:>12}",
        format!("filter: text=\"{}\"", text),
        median(&mut text_ms),
        format!("{} 命中", text_hits)
    );

    // 时间轴聚合
    let mut tl_ms = Vec::new();
    let mut buckets = 0usize;
    for _ in 0..runs {
        let t = std::time::Instant::now();
        let Some((lo, hi)) = ws.time_range() else {
            break;
        };
        let spec = logscope_core::timeline::spec_for_range(lo, hi);
        let _aggregated = logscope_core::timeline::aggregate(idx.entries.iter(), &spec);
        buckets = spec.count;
        tl_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    if !tl_ms.is_empty() {
        println!(
            "{:<24} {:>9.0}ms {:>12}",
            "timeline 聚合",
            median(&mut tl_ms),
            format!("{} 桶", buckets)
        );
    }

    // 分页摘要（模拟 UI 首屏 200 条）
    let mut page_ms = Vec::new();
    for _ in 0..runs {
        let t = std::time::Instant::now();
        let rs = logscope_core::search::SearchResultSet {
            refs: ws.time_order.clone(),
            scanned_bytes: 0,
        };
        let page = rs.page(0, 200);
        let _ = reader.read_summaries(
            &idx,
            &page.iter().map(|r| r.sequence_no).collect::<Vec<_>>(),
        )?;
        page_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "{:<24} {:>9.1}ms {:>12}",
        "page(200) 摘要读取",
        median(&mut page_ms),
        "200 条"
    );
    Ok(0)
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

// ---------------- preview ----------------

/// 解析预览：报告内置识别结果 / 自定义规则的逐行命中情况（不建索引）。
fn cmd_preview(file: &std::path::Path, pattern: Option<&str>, lines: usize) -> Result<i32> {
    let pv = logscope_core::parser::preview::preview_file(file, pattern, 480, lines)
        .with_context(|| format!("预览失败: {}", file.display()))?;
    match &pv.detected {
        Some(name) => println!("自动识别: {}", name),
        None => println!("使用自定义规则"),
    }
    println!("命中 {}/{} 行", pv.matched, pv.sampled);
    for r in &pv.rows {
        let mark = if r.matched { "✓" } else { "✗" };
        let ts = r
            .timestamp_ms
            .map(|m| fmt_ts(m, 480))
            .unwrap_or_else(|| "-".into());
        println!(
            "{} {} {:<6} {:<18} {:<28} {}",
            mark,
            ts,
            r.level.as_deref().unwrap_or("-"),
            r.thread.as_deref().unwrap_or("-"),
            r.logger.as_deref().unwrap_or("-"),
            r.line.chars().take(80).collect::<String>()
        );
    }
    if pv.matched == 0 {
        println!("\n没有一行被识别。请检查时间戳/级别格式，或用 --pattern 传入自定义正则。");
        return Ok(EXIT_EMPTY);
    }
    Ok(0)
}
