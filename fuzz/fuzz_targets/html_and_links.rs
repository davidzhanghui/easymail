//! 任意文本当 HTML：转纯文本、追踪像素识别、钓鱼链接 / 验证码 / 行程提取、去跟踪参数都不能 panic。
#![no_main]
use easymail::core::{extract, parse, utf7};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|s: &str| {
    let _ = parse::html_to_text(s);
    let _ = parse::tidy_text(s);
    let _ = parse::remote_trackers(s);
    let _ = extract::phishing_links(s);
    let _ = extract::verification_code(s, s);
    let _ = extract::trips(s, s);
    let _ = extract::strip_tracking(s);
    let _ = utf7::encode(&utf7::decode(s));
});
