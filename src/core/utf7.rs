//! IMAP 邮箱名的 modified UTF-7 编解码（RFC 3501 §5.1.3）。
//!
//! 数据库里存解码后的 UTF-8 路径（与 just-mail 一致），发给服务器时再编码。

use base64::alphabet::Alphabet;
use base64::engine::{GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;
use std::sync::LazyLock;

static B64: LazyLock<GeneralPurpose> = LazyLock::new(|| {
    let alpha = Alphabet::new("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,").expect("alphabet");
    GeneralPurpose::new(
        &alpha,
        GeneralPurposeConfig::new()
            .with_encode_padding(false)
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    )
});

pub fn decode(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(j) = after.find('-') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let chunk = &after[..j];
        if chunk.is_empty() {
            out.push('&');
        } else if let Ok(bytes) = B64.decode(chunk) {
            let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            out.push_str(&String::from_utf16_lossy(&units));
        } else {
            out.push('&');
            out.push_str(chunk);
            out.push('-');
        }
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    out
}

pub fn encode(s: &str) -> String {
    let mut out = String::new();
    let mut buf: Vec<u16> = Vec::new();
    let flush = |buf: &mut Vec<u16>, out: &mut String| {
        if !buf.is_empty() {
            let bytes: Vec<u8> = buf.iter().flat_map(|u| u.to_be_bytes()).collect();
            out.push('&');
            out.push_str(&B64.encode(bytes));
            out.push('-');
            buf.clear();
        }
    };
    for ch in s.chars() {
        let c = ch as u32;
        if (0x20..=0x7e).contains(&c) {
            flush(&mut buf, &mut out);
            if ch == '&' {
                out.push_str("&-");
            } else {
                out.push(ch);
            }
        } else {
            let mut tmp = [0u16; 2];
            buf.extend_from_slice(ch.encode_utf16(&mut tmp));
        }
    }
    flush(&mut buf, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_names() {
        // 实际从 Gmail / 163 / QQ 拿到的名字
        assert_eq!(decode("[Gmail]/&XfJT0ZCuTvY-"), "[Gmail]/已发邮件");
        assert_eq!(decode("&g0l6P3ux-"), "草稿箱");
        assert_eq!(decode("&XfJT0ZAB-"), "已发送");
        assert_eq!(decode("&UXZO1mWHTvZZOQ-/QQ&kK5O9ouilgU-"), "其他文件夹/QQ邮件订阅");
        assert_eq!(decode("&V4NXPpCuTvY-"), "垃圾邮件");
        assert_eq!(decode("INBOX"), "INBOX");
        assert_eq!(decode("A&-B"), "A&B");
    }

    #[test]
    fn roundtrip() {
        for s in ["[Gmail]/已发邮件", "草稿箱", "A&B", "Sent Messages", "其他文件夹/QQ邮件订阅", "😀 emoji", "~peter/mail/台北/日本語"] {
            assert_eq!(decode(&encode(s)), s, "{s}");
        }
        assert_eq!(encode("[Gmail]/已发邮件"), "[Gmail]/&XfJT0ZCuTvY-");
        assert_eq!(encode("A&B"), "A&-B");
    }
}
