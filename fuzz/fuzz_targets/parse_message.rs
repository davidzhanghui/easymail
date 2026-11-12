//! 任意字节当邮件原文：解析头、正文、附件、退订 / 认证头都不能 panic。
#![no_main]
use easymail::core::parse;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let env = parse::parse_headers(data);
    let _ = env.thread_key();
    if let Some(p) = parse::parse_full(data) {
        let _ = parse::inline_cids_from_raw(&p.html, data);
    }
    let _ = parse::file_attachments(data);
    let _ = parse::attachment(data, 0);
    let s = String::from_utf8_lossy(data);
    let _ = parse::unsub_targets(&s);
    let _ = parse::auth_verdict(&s);
});
