//! 从邮件里提取结构化信息：验证码、行程（航班 / 火车 / 酒店）、链接的跟踪参数、钓鱼链接提示。
//! 全部在本地用规则完成，不调用任何在线服务。

use regex::Regex;
use std::sync::LazyLock;

/// 验证码：主题或正文里出现「验证码 / 校验码 / 动态码 / code / OTP」等关键词，附近有 4–8 位数字（或字母数字混合）
pub fn verification_code(subject: &str, text: &str) -> Option<String> {
    static KEY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)(验证码|校验码|动态码|动态密码|确认码|安全码|登录码|verification code|security code|one[- ]time (?:pass)?code|passcode|\bOTP\b|\bcode\b)",
        )
        .expect("regex")
    });
    static NUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|[^0-9A-Za-z])([0-9]{4,8}|[A-Z0-9]{6})(?:$|[^0-9A-Za-z])").expect("regex"));
    let body: String = text.chars().take(4000).collect();
    for src in [subject, body.as_str()] {
        for m in KEY.find_iter(src) {
            // 关键词后 80 个字符内的第一个候选；没有再看前 40 个字符
            let after: String = src[m.end()..].chars().take(80).collect();
            let before: String = {
                let b: Vec<char> = src[..m.start()].chars().collect();
                b[b.len().saturating_sub(40)..].iter().collect()
            };
            for seg in [after, before] {
                for c in NUM.captures_iter(&seg) {
                    let code = c[1].to_string();
                    // 排除年份、纯字母
                    if code.chars().any(|c| c.is_ascii_digit())
                        && !(code.len() == 4 && (code.starts_with("19") || code.starts_with("20")) && seg.contains('年'))
                    {
                        return Some(code);
                    }
                }
            }
        }
    }
    // 主题里说了是验证码、正文却只有数字（"Use 7731 to sign in"）：取正文开头的第一个候选
    if KEY.is_match(subject) {
        let head: String = body.chars().take(500).collect();
        if let Some(c) = NUM.captures_iter(&head).map(|c| c[1].to_string()).find(|c| c.chars().all(|x| x.is_ascii_digit())) {
            return Some(c);
        }
    }
    None
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trip {
    /// flight / train / hotel
    pub kind: &'static str,
    /// 航班号 / 车次 / 酒店名
    pub title: String,
    /// 出发地 → 目的地 / 地址
    pub route: String,
    /// 时间（原文）
    pub when: String,
    /// 订单号 / 确认号
    pub reference: String,
}

/// 行程：优先读 schema.org 的 JSON-LD（航司、OTA 的确认信大多带），其次用中文票务邮件的常见格式兜底
pub fn trips(html: &str, text: &str) -> Vec<Trip> {
    let mut out = json_ld_trips(html);
    if out.is_empty() {
        out = text_trips(text);
    }
    out.truncate(6);
    out
}

fn json_ld_trips(html: &str) -> Vec<Trip> {
    static LD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?is)<script[^>]+type\s*=\s*["']application/ld\+json["'][^>]*>(.*?)</script>"#).expect("regex"));
    let mut out = Vec::new();
    for c in LD.captures_iter(html) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(c[1].trim()) else { continue };
        let items: Vec<serde_json::Value> = match v {
            serde_json::Value::Array(a) => a,
            serde_json::Value::Object(ref o) if o.contains_key("@graph") => o["@graph"].as_array().cloned().unwrap_or_default(),
            other => vec![other],
        };
        for it in items {
            if let Some(t) = trip_of(&it) {
                out.push(t);
            }
        }
    }
    out
}

fn s(v: &serde_json::Value, path: &[&str]) -> String {
    let mut cur = v;
    for p in path {
        match cur.get(p) {
            Some(n) => cur = n,
            None => return String::new(),
        }
    }
    match cur {
        serde_json::Value::String(x) => x.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn trip_of(v: &serde_json::Value) -> Option<Trip> {
    let ty = s(v, &["@type"]);
    let reference = s(v, &["reservationNumber"]);
    match ty.as_str() {
        "FlightReservation" => {
            let f = v.get("reservationFor")?;
            let title = format!("{}{}", s(f, &["airline", "iataCode"]), s(f, &["flightNumber"]));
            let from = [s(f, &["departureAirport", "name"]), s(f, &["departureAirport", "iataCode"])].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
            let to = [s(f, &["arrivalAirport", "name"]), s(f, &["arrivalAirport", "iataCode"])].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
            Some(Trip { kind: "flight", title, route: format!("{from} → {to}"), when: s(f, &["departureTime"]), reference })
        }
        "TrainReservation" => {
            let f = v.get("reservationFor")?;
            Some(Trip {
                kind: "train",
                title: s(f, &["trainNumber"]),
                route: format!("{} → {}", s(f, &["departureStation", "name"]), s(f, &["arrivalStation", "name"])),
                when: s(f, &["departureTime"]),
                reference,
            })
        }
        "LodgingReservation" => {
            let f = v.get("reservationFor")?;
            Some(Trip { kind: "hotel", title: s(f, &["name"]), route: s(f, &["address", "streetAddress"]), when: s(v, &["checkinTime"]), reference })
        }
        _ => None,
    }
}

fn text_trips(text: &str) -> Vec<Trip> {
    static FLIGHT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:航班号?|Flight)[:：\s]*([A-Z0-9]{2}\s?\d{3,4})[\s\S]{0,80}?(\d{4}[-/年]\d{1,2}[-/月]\d{1,2}日?\s*\d{1,2}:\d{2})?").expect("regex")
    });
    static TRAIN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"([GDCZTKY]\d{1,4})次?[^\n]{0,20}?([\p{Han}]{2,8})站?\s*[-—至到→]+\s*([\p{Han}]{2,8})站?[^\n]{0,40}?(\d{1,2}月\d{1,2}日\s*\d{1,2}:\d{2}|\d{4}-\d{2}-\d{2}\s*\d{1,2}:\d{2})?").expect("regex")
    });
    static ORDER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:订单号|订单编号|确认号|预订号|Booking (?:ref|reference))[:：\s]*([A-Z0-9-]{5,})").expect("regex"));
    let t: String = text.chars().take(6000).collect();
    let reference = ORDER.captures(&t).map(|c| c[1].to_string()).unwrap_or_default();
    let mut out = Vec::new();
    if t.contains("航班") || t.to_lowercase().contains("flight") {
        for c in FLIGHT.captures_iter(&t).take(3) {
            out.push(Trip {
                kind: "flight",
                title: c[1].replace(' ', ""),
                route: String::new(),
                when: c.get(2).map(|m| m.as_str().to_string()).unwrap_or_default(),
                reference: reference.clone(),
            });
        }
    }
    if t.contains("车次") || t.contains("12306") || t.contains("检票") {
        for c in TRAIN.captures_iter(&t).take(3) {
            out.push(Trip {
                kind: "train",
                title: c[1].to_string(),
                route: format!("{} → {}", c[2].trim_end_matches('站'), c[3].trim_end_matches('站')),
                when: c.get(4).map(|m| m.as_str().to_string()).unwrap_or_default(),
                reference: reference.clone(),
            });
        }
    }
    out
}

/// 去掉链接里的跟踪参数（utm_*、fbclid、gclid、spm 等），其余参数保持原样
pub fn strip_tracking(url: &str) -> String {
    const DROP: [&str; 14] =
        ["fbclid", "gclid", "dclid", "msclkid", "mc_cid", "mc_eid", "yclid", "igshid", "_hsenc", "_hsmi", "mkt_tok", "spm", "vero_id", "oly_enc_id"];
    let Ok(mut u) = url::Url::parse(url) else { return url.to_string() };
    if u.query().is_none() {
        return url.to_string();
    }
    let kept: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| !k.to_ascii_lowercase().starts_with("utm_") && !DROP.contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        u.set_query(None);
    } else {
        u.query_pairs_mut().clear().extend_pairs(kept);
    }
    u.to_string()
}

/// 钓鱼链接提示：链接文字本身像网址、但和真实地址的域名不同（例如显示 paypal.com，实际指向 paypa1-login.xyz）
pub fn phishing_links(html: &str) -> Vec<(String, String)> {
    static A: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?is)<a\b[^>]*\bhref\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a>"#).expect("regex"));
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").expect("regex"));
    static DOMAINISH: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^(?:https?://)?(?:www\.)?([a-z0-9-]+(?:\.[a-z0-9-]+)+)(?:[/:?#].*)?$").expect("regex"));
    let mut out = Vec::new();
    for c in A.captures_iter(html) {
        let href = c[1].trim();
        let text = TAG.replace_all(&c[2], "").trim().to_string();
        let Some(shown) = DOMAINISH.captures(&text).map(|m| m[1].to_lowercase()) else { continue };
        let Some(real) = url::Url::parse(href).ok().and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_lowercase())) else { continue };
        if !same_site(&shown, &real) && !out.iter().any(|(s, r): &(String, String)| s == &shown && r == &real) {
            out.push((shown, real));
        }
    }
    out
}

/// 粗略的「同一站点」判断：一方是另一方的子域名就算同一个
fn same_site(a: &str, b: &str) -> bool {
    a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        assert_eq!(verification_code("您的验证码", "验证码：482913，5 分钟内有效"), Some("482913".into()));
        assert_eq!(verification_code("Your code", "Use 7731 to sign in"), Some("7731".into()));
        assert_eq!(verification_code("Verify", "Your verification code is A7K9Q2."), Some("A7K9Q2".into()));
        assert_eq!(verification_code("周报", "本周销售额 123456 元"), None);
        assert_eq!(verification_code("", "123456 是您的动态码"), Some("123456".into()));
    }

    #[test]
    fn trips_json_ld_and_text() {
        let html = r#"<script type="application/ld+json">{"@type":"FlightReservation","reservationNumber":"ABC123","reservationFor":{"flightNumber":"1234","airline":{"iataCode":"MU"},"departureAirport":{"iataCode":"PVG"},"arrivalAirport":{"iataCode":"PEK"},"departureTime":"2025-03-01T08:00:00+08:00"}}</script>"#;
        let t = trips(html, "");
        assert_eq!(
            t[0],
            Trip { kind: "flight", title: "MU1234".into(), route: "PVG → PEK".into(), when: "2025-03-01T08:00:00+08:00".into(), reference: "ABC123".into() }
        );
        let t = trips("", "订单号：E123456789 您已购买 G1234次 上海虹桥站-北京南站 3月1日 08:00 开，检票口 A1");
        assert_eq!(t[0].kind, "train");
        assert_eq!(t[0].title, "G1234");
        assert_eq!(t[0].route, "上海虹桥 → 北京南");
        assert_eq!(t[0].reference, "E123456789");
    }

    #[test]
    fn tracking_and_phishing() {
        assert_eq!(strip_tracking("https://a.com/p?id=1&utm_source=x&fbclid=y"), "https://a.com/p?id=1");
        assert_eq!(strip_tracking("https://a.com/p?utm_medium=e"), "https://a.com/p");
        assert_eq!(strip_tracking("https://a.com/p"), "https://a.com/p");
        let h = r#"<a href="https://paypa1-login.xyz/x">www.paypal.com</a><a href="https://mail.google.com/">google.com</a><a href="https://x.com">点这里</a>"#;
        assert_eq!(phishing_links(h), vec![("paypal.com".into(), "paypa1-login.xyz".into())]);
    }
}
