//! 时间戳解析。
//!
//! 时间形状已由 Profile 正则保证（`YYYY-MM-DD[ T]HH:MM:SS[.1-9 位小数]`），
//! 这里做手工定长拆位 + chrono 日历换算，比 format 串快约一个量级。
//! 闰年 / 月长 / 时分秒范围的正确性仍由 chrono 的 `*_opt` 构造器保证。

/// 时间戳解析器。
pub struct TimeParser {
    /// 无时区信息时使用的工作区默认时区（分钟偏移），默认 480（Asia/Shanghai）
    pub default_tz_offset_min: i32,
}

impl TimeParser {
    /// 解析时间戳。
    ///
    /// 入参：
    /// - `ts`：正则 `ts` 组字节（形状已保证）
    /// - `tz`：正则 `tz` 组（`Z` / `+HH:MM` / `+HHMM`），无则为 `None`
    ///
    /// 返回 `(utc_ms, 使用的时区偏移分钟, 是否结构匹配但值非法)`。
    /// 值非法时 `utc_ms` 为 `None`（事件保留，归入未知时间分组）。
    pub fn parse(&self, ts: &[u8], tz: Option<&[u8]>) -> (Option<i64>, Option<i16>, bool) {
        let (naive_ms, bad) = parse_naive_ms(ts);
        if bad {
            return (None, None, true);
        }
        let (offset_min, offset_bad) = match tz {
            Some(tz_bytes) => parse_tz(tz_bytes),
            None => (self.default_tz_offset_min as i64, false),
        };
        if offset_bad {
            return (None, None, true);
        }
        let utc_ms = naive_ms - offset_min * 60_000;
        (Some(utc_ms), Some(offset_min as i16), false)
    }
}

/// 手工定长拆位：`YYYY-MM-DD[ T]HH:MM:SS[.f{1,9}]` → 本地墙钟毫秒。
/// 兼容日期分隔符 `-` / `/`，小数分隔符 `.` / `,`。
fn parse_naive_ms(ts: &[u8]) -> (i64, bool) {
    // 最短合法长度：19（无小数）
    if ts.len() < 19 {
        return (0, true);
    }
    let y = num(&ts[0..4]);
    let mo = num(&ts[5..7]);
    let d = num(&ts[8..10]);
    let h = num(&ts[11..13]);
    let mi = num(&ts[14..16]);
    let s = num(&ts[17..19]);
    if y.is_none() || mo.is_none() || d.is_none() || h.is_none() || mi.is_none() || s.is_none() {
        return (0, true);
    }
    // 分隔符形状校验（防 `captures` 与形状假设不符的防御性检查）
    if !matches!(ts[4], b'-' | b'/')
        || !matches!(ts[7], b'-' | b'/')
        || !matches!(ts[10], b' ' | b'T')
        || ts[13] != b':'
        || ts[16] != b':'
    {
        return (0, true);
    }

    // 可选小数：从 19 位后
    let mut millis = 0i64;
    if ts.len() > 19 {
        if !matches!(ts[19], b'.' | b',') {
            return (0, true);
        }
        let frac = &ts[20..];
        if frac.is_empty() || frac.len() > 9 || !frac.iter().all(u8::is_ascii_digit) {
            return (0, true);
        }
        // 折算为毫秒（3 位截断后右补零）：123 → 123ms；1234 → 123ms；1 → 100ms
        let n = frac.len().min(3);
        let mut padded = [b'0'; 3];
        padded[..n].copy_from_slice(&frac[..n]);
        millis = num(&padded).unwrap_or(0);
    }

    let date =
        chrono::NaiveDate::from_ymd_opt(y.unwrap() as i32, mo.unwrap() as u32, d.unwrap() as u32);
    let time =
        chrono::NaiveTime::from_hms_opt(h.unwrap() as u32, mi.unwrap() as u32, s.unwrap() as u32);
    let (date, time) = match (date, time) {
        (Some(d), Some(t)) => (d, t),
        _ => return (0, true), // 非法日期（如 2026-13-45、闰日错误）
    };
    let dt = date.and_time(time);
    (dt.and_utc().timestamp_millis() + millis, false)
}

/// 解析时区组：`Z` / `+HH:MM` / `-HH:MM` / `+HHMM` / `-HHMM` → 分钟偏移。
fn parse_tz(tz: &[u8]) -> (i64, bool) {
    match tz.first() {
        Some(b'Z') if tz.len() == 1 => (0, false),
        Some(sign @ (b'+' | b'-')) => {
            let digits: Vec<u8> = tz[1..]
                .iter()
                .copied()
                .filter(|b| b.is_ascii_digit())
                .collect();
            if digits.len() != 4 {
                return (0, true);
            }
            let hh = num(&digits[0..2]);
            let mm = num(&digits[2..4]);
            if hh.is_none() || mm.is_none() {
                return (0, true);
            }
            let mag = hh.unwrap() * 60 + mm.unwrap();
            if hh.unwrap() > 23 || mm.unwrap() > 59 {
                return (0, true);
            }
            if *sign == b'-' {
                (-mag, false)
            } else {
                (mag, false)
            }
        }
        _ => (0, true),
    }
}

/// ASCII 数字串 → 数值
fn num(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let mut v: i64 = 0;
    for b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (b - b'0') as i64;
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(default_tz: i32) -> TimeParser {
        TimeParser {
            default_tz_offset_min: default_tz,
        }
    }

    #[test]
    fn basic_no_tz_uses_default() {
        let (ms, off, bad) = p(480).parse(b"2026-11-20 09:15:42.123", None);
        assert!(!bad);
        assert_eq!(off, Some(480));
        // 2026-11-20 09:15:42.123 +08:00 = 01:15:42.123 UTC
        let dt = chrono::DateTime::from_timestamp_millis(ms.unwrap()).unwrap();
        assert_eq!(dt.to_string(), "2026-11-20 01:15:42.123 UTC");
    }

    #[test]
    fn explicit_tz_wins() {
        let (ms, off, _) = p(480).parse(b"2026-11-20 09:15:42", Some(b"Z"));
        assert_eq!(off, Some(0));
        let dt = chrono::DateTime::from_timestamp_millis(ms.unwrap()).unwrap();
        assert_eq!(dt.to_string(), "2026-11-20 09:15:42 UTC");

        // 同一瞬间的两种写法：09:15:42+08:00 == 01:15:42Z
        let (ms2, off2, _) = p(0).parse(b"2026-11-20 09:15:42", Some(b"+08:00"));
        assert_eq!(off2, Some(480));
        let (ms3, _, _) = p(480).parse(b"2026-11-20 01:15:42", Some(b"Z"));
        assert_eq!(ms2, ms3, "09:15:42+08:00 应等于 01:15:42Z");

        // 无时区走默认
        let (ms4, off4, _) = p(480).parse(b"2026-11-20 09:15:42", None);
        assert_eq!(off4, Some(480));
        assert_eq!(ms4, ms2, "默认 +480 应与显式 +08:00 一致");
    }

    #[test]
    fn tz_compact_form() {
        let (a, _, _) = p(480).parse(b"2026-11-20 09:15:42", Some(b"+0800"));
        let (b, _, _) = p(480).parse(b"2026-11-20 09:15:42", Some(b"+08:00"));
        assert_eq!(a, b);
        let (neg, off, _) = p(0).parse(b"2026-11-20 09:15:42", Some(b"-05:30"));
        assert_eq!(off, Some(-330));
        assert!(neg.is_some());
    }

    #[test]
    fn t_separator_iso8601() {
        let (ms, _, bad) = p(0).parse(b"2026-11-20T09:15:42.5", None);
        assert!(!bad);
        let dt = chrono::DateTime::from_timestamp_millis(ms.unwrap()).unwrap();
        assert_eq!(dt.to_string(), "2026-11-20 09:15:42.500 UTC");
    }

    #[test]
    fn fraction_variants() {
        let (m1, _, _) = p(0).parse(b"2026-11-20 00:00:00.1", None);
        assert_eq!(m1.unwrap() % 1000, 100);
        let (m2, _, _) = p(0).parse(b"2026-11-20 00:00:00.123456789", None);
        assert_eq!(m2.unwrap() % 1000, 123);
        let (m3, _, _) = p(0).parse(b"2026-11-20 00:00:00", None);
        assert_eq!(m3.unwrap() % 1000, 0);
    }

    #[test]
    fn invalid_calendar_dates() {
        for bad_ts in [
            &b"2026-13-45 09:15:42"[..], // 月、日非法
            &b"2026-02-30 09:15:42"[..], // 非闰月日
            &b"2027-02-29 09:15:42"[..], // 平年闰日
            &b"2026-11-20 25:00:00"[..], // 时越界
            &b"2026-11-20 09:60:00"[..], // 分越界
        ] {
            let (ms, _, bad) = p(0).parse(bad_ts, None);
            assert!(bad, "{:?} 应为非法", String::from_utf8_lossy(bad_ts));
            assert!(ms.is_none());
        }
        // 闰年闰日合法
        let (ms, _, bad) = p(0).parse(b"2028-02-29 00:00:00", None);
        assert!(!bad);
        assert!(ms.is_some());
    }

    #[test]
    fn bad_tz() {
        let (_, _, bad) = p(0).parse(b"2026-11-20 09:15:42", Some(b"+99:00"));
        assert!(bad);
        let (_, _, bad2) = p(0).parse(b"2026-11-20 09:15:42", Some(b"+8:00"));
        assert!(bad2);
    }
}
