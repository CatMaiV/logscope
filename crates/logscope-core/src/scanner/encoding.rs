//! 编码探测与展示层解码。
//!
//! 解析在字节层进行；编码仅影响：(a) 非 UTF-8 文件的 lossy 提示，(b) 展示层解码。

use crate::model::error::CoreError;
use crate::model::file_meta::SourceEncoding;

/// UTF-8 BOM
const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// BOM 探测。
/// - UTF-8 BOM → `Utf8Bom`
/// - 无 BOM → `Utf8`（后续由采样修正为 Gbk / LossyUtf8）
/// - UTF-16 LE/BE BOM → `Err(UnsupportedEncoding)`（V1.0 不支持，引导用户转存）
pub fn detect_bom(prefix: &[u8]) -> Result<SourceEncoding, CoreError> {
    if prefix.starts_with(&UTF8_BOM) {
        return Ok(SourceEncoding::Utf8Bom);
    }
    if prefix.len() >= 2 {
        let le = prefix[0] == 0xFF && prefix[1] == 0xFE;
        let be = prefix[0] == 0xFE && prefix[1] == 0xFF;
        if le || be {
            return Err(CoreError::UnsupportedEncoding(
                if le {
                    "UTF-16 LE（含 BOM）"
                } else {
                    "UTF-16 BE（含 BOM）"
                }
                .to_string(),
            ));
        }
    }
    Ok(SourceEncoding::Utf8)
}

/// 采样块的 UTF-8 健全性。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Utf8Health {
    /// 全部合法 UTF-8
    Valid,
    /// 含非法字节（可能是 GBK 等本地编码）
    Invalid,
    /// 采样不足以判定
    Insufficient,
}

/// 检查采样字节是否为合法 UTF-8。
/// 使用 `std::str::from_utf8`；尾部截断的多字节序列（块边界）视为合法。
pub fn utf8_health(sample: &[u8]) -> Utf8Health {
    if sample.len() < 4 {
        return Utf8Health::Insufficient;
    }
    match std::str::from_utf8(sample) {
        Ok(_) => Utf8Health::Valid,
        Err(e) => match e.error_len() {
            // 存在确定非法的字节 → 不是 UTF-8
            Some(_) => Utf8Health::Invalid,
            // None：仅尾部多字节序列被采样截断，前缀合法 → 容忍
            None => Utf8Health::Valid,
        },
    }
}

/// 展示层解码：字节 → 字符串。
/// GBK 用 `encoding_rs` 解码；其他情况 UTF-8（失败时有损替换，不静默丢内容）。
pub fn decode_for_display(bytes: &[u8], enc: SourceEncoding) -> String {
    match enc {
        SourceEncoding::Utf8 | SourceEncoding::Utf8Bom | SourceEncoding::LossyUtf8 => {
            String::from_utf8_lossy(bytes).into_owned()
        }
        SourceEncoding::Gbk => {
            let (cow, _, had_errors) = encoding_rs::GBK.decode(bytes);
            if had_errors {
                String::from_utf8_lossy(bytes).into_owned()
            } else {
                cow.into_owned()
            }
        }
    }
}

/// 采样修正编码判定：UTF-8 不健康时按 GBK 处理（中文 Windows 环境最常见）。
/// GBK 与 ASCII 兼容，日志头的正则识别不受影响。
pub fn refine_encoding(sample: &[u8], current: SourceEncoding) -> SourceEncoding {
    match current {
        SourceEncoding::Utf8 => match utf8_health(sample) {
            Utf8Health::Invalid => SourceEncoding::Gbk,
            _ => current,
        },
        SourceEncoding::Gbk => match utf8_health(sample) {
            Utf8Health::Valid => SourceEncoding::Utf8,
            _ => current,
        },
        _ => current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bom_utf8() {
        assert_eq!(detect_bom(&UTF8_BOM).unwrap(), SourceEncoding::Utf8Bom);
        assert_eq!(
            detect_bom(b"\xEF\xBB\xBFxx").unwrap(),
            SourceEncoding::Utf8Bom
        );
    }

    #[test]
    fn bom_utf16_rejected() {
        assert!(detect_bom(&[0xFF, 0xFE, 0x00, 0x00]).is_err());
        assert!(detect_bom(&[0xFE, 0xFF]).is_err());
    }

    #[test]
    fn no_bom_is_utf8() {
        assert_eq!(detect_bom(b"2026-11-20").unwrap(), SourceEncoding::Utf8);
        assert_eq!(detect_bom(b"").unwrap(), SourceEncoding::Utf8);
    }

    #[test]
    fn health_checks() {
        assert_eq!(utf8_health(b"hello world"), Utf8Health::Valid);
        assert_eq!(utf8_health("中文日志".as_bytes()), Utf8Health::Valid);
        // GBK 编码的"中文"在 UTF-8 下非法
        assert_eq!(utf8_health(&[0xD6, 0xD0, 0xCE, 0xC4]), Utf8Health::Invalid);
        // 尾部截断的多字节序列：容忍
        assert_eq!(utf8_health(&"中文".as_bytes()[..4]), Utf8Health::Valid);
    }

    #[test]
    fn decode_gbk() {
        // GBK "中文" = D6 D0 CE C4
        let s = decode_for_display(&[0xD6, 0xD0, 0xCE, 0xC4], SourceEncoding::Gbk);
        assert_eq!(s, "中文");
    }

    #[test]
    fn refine_paths() {
        assert_eq!(
            refine_encoding(&[0xD6, 0xD0, 0xCE, 0xC4], SourceEncoding::Utf8),
            SourceEncoding::Gbk
        );
        assert_eq!(
            refine_encoding("正常".as_bytes(), SourceEncoding::Utf8),
            SourceEncoding::Utf8
        );
    }
}
