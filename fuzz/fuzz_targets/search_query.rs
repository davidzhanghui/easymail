//! 任意搜索输入：语法解析不能 panic。
#![no_main]
use easymail::core::queries;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|s: &str| {
    let _ = queries::parse_query(s);
});
