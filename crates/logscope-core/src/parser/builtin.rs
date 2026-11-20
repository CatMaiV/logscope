//! 内置 Logback 解析器注册表（顺序即优先级）。

use crate::model::level::LogLevel;
use crate::model::profile::{BuiltinProfile, EventParser, ParsedHead, ProfileId, RawParser};
use crate::parser::time::TimeParser;
use std::sync::OnceLock;

/// (1) Spring Boot 默认：`%d %5p ${PID} --- [%t] %logger{39} : %m`
/// 形如 `2026-11-20 09:15:42.123  INFO 12345 --- [main] com.example.Foo : msg`
const RE_SPRINGBOOT: &str = r"(?-u)^(?P<ts>\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?)(?P<tz>Z|[+-]\d{2}:?\d{2})?\s+(?P<level>TRACE|DEBUG|INFO|WARN|ERROR|FATAL)\s+(?:\S+\s+)?---\s+\[(?P<thread>[^\]]*)\]\s+(?P<logger>\S+)\s*:\s?(?P<msg>.*)$";

/// (2) Logback 标准：`%d{...} [%thread] %-5level %logger{36} - %msg`
const RE_LOGBACK_STD: &str = r"(?-u)^(?P<ts>\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?)(?P<tz>Z|[+-]\d{2}:?\d{2})?\s+\[(?P<thread>[^\]]*)\]\s+(?P<level>TRACE|DEBUG|INFO|WARN|ERROR|FATAL)\s+(?P<logger>\S+)\s+-\s?(?P<msg>.*)$";

/// (3) Logback 无线程段变体：`%d{...} %-5level %logger - %msg`
const RE_LOGBACK_NOTHREAD: &str = r"(?-u)^(?P<ts>\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?)(?P<tz>Z|[+-]\d{2}:?\d{2})?\s+(?P<level>TRACE|DEBUG|INFO|WARN|ERROR|FATAL)\s+(?P<logger>\S+)\s+-\s?(?P<msg>.*)$";

/// 基于正则的事件头解析器。三个内置 Pattern 共享同一组命名捕获组。
pub struct RegexHeadParser {
    id: ProfileId,
    name: &'static str,
    re: regex::bytes::Regex,
}

impl RegexHeadParser {
    fn new(id: ProfileId, name: &'static str, pattern: &str) -> Self {
        RegexHeadParser {
            id,
            name,
            re: regex::bytes::Regex::new(pattern).expect("内置正则必须编译成功"),
        }
    }
}

impl EventParser for RegexHeadParser {
    fn id(&self) -> ProfileId {
        self.id
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn is_head(&self, line: &[u8]) -> bool {
        self.re.is_match(line)
    }

    fn parse_head(&self, line: &[u8], time: &TimeParser) -> ParsedHead {
        let caps = match self.re.captures(line) {
            Some(c) => c,
            None => {
                // 调用方保证 is_head 先行；防御性兜底
                return ParsedHead {
                    level: LogLevel::Unknown,
                    logger: None,
                    thread: None,
                    timestamp_ms: None,
                    tz_offset_min: None,
                    bad_timestamp: false,
                };
            }
        };
        let level = caps
            .name("level")
            .and_then(|m| LogLevel::from_bytes(m.as_bytes()))
            .unwrap_or(LogLevel::Unknown);
        let (timestamp_ms, tz_offset_min, bad_timestamp) = match caps.name("ts") {
            Some(ts) => {
                let tz = caps.name("tz").map(|m| m.as_bytes());
                let (ms, off, bad) = time.parse(ts.as_bytes(), tz);
                (ms, off, bad)
            }
            None => (None, None, false),
        };
        let logger = caps
            .name("logger")
            .map(|m| (m.start() as u64, m.end() as u64));
        let thread = caps
            .name("thread")
            .map(|m| (m.start() as u64, m.end() as u64));
        ParsedHead {
            level,
            logger,
            thread,
            timestamp_ms,
            tz_offset_min,
            bad_timestamp,
        }
    }
}

type BoxedParser = Box<dyn EventParser>;

/// 内置解析器注册表（顺序即 detect 优先级）。
pub fn builtin_parsers() -> &'static [BoxedParser] {
    static REGISTRY: OnceLock<Vec<BoxedParser>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        vec![
            Box::new(RegexHeadParser::new(
                ProfileId::Builtin(BuiltinProfile::SpringBootDefault),
                "springboot",
                RE_SPRINGBOOT,
            )),
            Box::new(RegexHeadParser::new(
                ProfileId::Builtin(BuiltinProfile::LogbackStd),
                "logback-std",
                RE_LOGBACK_STD,
            )),
            Box::new(RegexHeadParser::new(
                ProfileId::Builtin(BuiltinProfile::LogbackNoThread),
                "logback-nothread",
                RE_LOGBACK_NOTHREAD,
            )),
            Box::new(RawParser),
        ]
    })
}

/// 按 id 查找内置解析器。
pub fn find_parser(id: &ProfileId) -> &'static dyn EventParser {
    for p in builtin_parsers() {
        if p.id() == *id {
            return p.as_ref();
        }
    }
    // 未知 id 兜底为 Raw
    builtin_parsers().last().expect("注册表非空").as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn springboot() -> &'static dyn EventParser {
        find_parser(&ProfileId::Builtin(BuiltinProfile::SpringBootDefault))
    }
    fn std_() -> &'static dyn EventParser {
        find_parser(&ProfileId::Builtin(BuiltinProfile::LogbackStd))
    }
    fn nothread() -> &'static dyn EventParser {
        find_parser(&ProfileId::Builtin(BuiltinProfile::LogbackNoThread))
    }

    #[test]
    fn springboot_default_matches() {
        let line = b"2026-11-20 09:15:42.123  INFO 12345 --- [main] com.example.Foo : started in 3.2 seconds";
        assert!(springboot().is_head(line));
        let head = springboot().parse_head(
            line,
            &TimeParser {
                default_tz_offset_min: 480,
            },
        );
        assert_eq!(head.level, LogLevel::Info);
        assert!(head.timestamp_ms.is_some());
        // logger 相对区间
        let (s, e) = head.logger.unwrap();
        assert_eq!(&line[s as usize..e as usize], b"com.example.Foo");
    }

    #[test]
    fn springboot_error_padding() {
        // %5p 右对齐：ERROR 前 1 空格；无 PID（可选组缺失）
        let line = b"2026-11-20 09:15:42.999 ERROR --- [http-nio-8080-exec-1] o.a.c.c.C.[.[.[/].[dispatcherServlet] : Servlet.service()"
            ;
        assert!(springboot().is_head(line));
        let head = springboot().parse_head(
            line,
            &TimeParser {
                default_tz_offset_min: 480,
            },
        );
        assert_eq!(head.level, LogLevel::Error);
    }

    #[test]
    fn logback_std_matches_and_padding() {
        // %-5level 左对齐：INFO 后 1 空格
        let line = b"2026-11-20 09:15:42.123 [main] INFO  com.example.Bar - app started";
        assert!(std_().is_head(line));
        let head = std_().parse_head(
            line,
            &TimeParser {
                default_tz_offset_min: 480,
            },
        );
        assert_eq!(head.level, LogLevel::Info);
        assert_eq!(
            &line[head.logger.unwrap().0 as usize..head.logger.unwrap().1 as usize],
            b"com.example.Bar"
        );
    }

    #[test]
    fn logback_std_iso8601_tz() {
        let line = b"2026-11-20T09:15:42,123+08:00 [worker-1] WARN  c.e.Foo - disk nearly full";
        // 注意：逗号小数分隔（欧洲习惯）不在支持集合内——此行 ts 组停在秒
        // 逗号不会被 tz 组吃掉（tz 以 Z/+/- 开头），后续 \s+ 失败 → 不是 head
        assert!(!std_().is_head(line));
    }

    #[test]
    fn logback_std_iso8601_dot() {
        let line = b"2026-11-20T09:15:42.123Z [worker-1] WARN  c.e.Foo - disk nearly full";
        assert!(std_().is_head(line));
        let head = std_().parse_head(
            line,
            &TimeParser {
                default_tz_offset_min: 480,
            },
        );
        assert_eq!(head.tz_offset_min, Some(0));
    }

    #[test]
    fn no_thread_variant() {
        let line = b"2026-11-20 09:15:42 ERROR com.example.Baz - boom";
        assert!(!std_().is_head(line));
        assert!(nothread().is_head(line));
        let head = nothread().parse_head(
            line,
            &TimeParser {
                default_tz_offset_min: 480,
            },
        );
        assert_eq!(head.level, LogLevel::Error);
        assert!(head.timestamp_ms.is_some());
    }

    #[test]
    fn continuation_lines_not_head() {
        for cont in [
            &b"    at com.example.Foo.bar(Foo.java:42)"[..],
            &b"Caused by: java.lang.NullPointerException: null"[..],
            &b"\tSuppressed: java.io.IOException"[..],
            &b"\t... 42 common frames omitted"[..],
            &b""[..],
            &b"random noise without timestamp"[..],
        ] {
            assert!(!std_().is_head(cont), "{:?} 不应是头", cont);
            assert!(!springboot().is_head(cont));
            assert!(!nothread().is_head(cont));
        }
    }

    #[test]
    fn no_millis_variant() {
        let line = b"2026-11-20 09:15:42 [main] DEBUG c.e.Foo - without millis";
        assert!(std_().is_head(line));
    }

    #[test]
    fn raw_never_matches() {
        let raw = find_parser(&ProfileId::Builtin(BuiltinProfile::Raw));
        assert!(!raw.is_head(b"2026-11-20 09:15:42 [main] INFO c.e.Foo - x"));
        assert!(!raw.is_head(b"anything"));
    }

    #[test]
    fn gbk_bytes_still_match() {
        // GBK 编码的 logger/消息（非法 UTF-8 字节序列）必须仍能识别为头行。
        // 回归背景：regex::bytes 默认 Unicode 语义下 \S 遇非法 UTF-8 会失配。
        let line: Vec<u8> = b"2026-11-20 09:00:00.000 [main] INFO  c.e."
            .iter()
            .chain(&[0xD6, 0xD0, 0xCE, 0xC4]) // GBK "中文"
            .chain(b" - \xD6\xD0\xCE\xC4")
            .copied()
            .collect();
        assert!(std_().is_head(&line), "GBK 行应匹配（字节语义）");
        let head = std_().parse_head(
            &line,
            &TimeParser {
                default_tz_offset_min: 480,
            },
        );
        assert_eq!(head.level, LogLevel::Info);
        let (s, e) = head.logger.unwrap();
        assert_eq!(&line[s as usize..e as usize], b"c.e.\xD6\xD0\xCE\xC4");
    }

    #[test]
    fn find_parser_fallback() {
        let unknown = find_parser(&ProfileId::Custom(999));
        assert_eq!(unknown.id(), ProfileId::Builtin(BuiltinProfile::Raw));
    }
}
