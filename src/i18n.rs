//! 界面语言：源码里的文案是简体中文，英文界面时在「文字落到界面上」的地方（按钮、菜单、标题、提示条…）
//! 用 `locales/en.tsv`（中文 ⇥ 英文，一行一条）查表翻译；查不到就原样显示中文。
//!
//! - 精确匹配：整句相同；
//! - 模板匹配：带 `{}` 的条目对应 `format!` 拼出来的句子，`{}` 处的内容原样（或递归翻译后）代入英文模板；
//! - 语言设置存在 kv `lang`：`auto`（默认，跟随系统）/ `zh` / `en`。

use crate::core::db::Store;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::LazyLock;

static LANG: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

struct Pattern {
    parts: Vec<String>,
    en: String,
}

struct Dict {
    exact: HashMap<String, String>,
    patterns: Vec<Pattern>,
}

static DICT: LazyLock<Dict> = LazyLock::new(|| parse_dict(include_str!("../locales/en.tsv")));

fn parse_dict(src: &str) -> Dict {
    let mut exact = HashMap::new();
    let mut patterns = Vec::new();
    for line in src.lines() {
        let Some((zh, en)) = line.split_once('\t') else { continue };
        if zh.contains("{}") {
            patterns.push(Pattern { parts: zh.split("{}").map(String::from).collect(), en: en.to_string() });
        } else {
            exact.insert(zh.to_string(), en.to_string());
        }
    }
    // 固定文字更多的模板优先（更具体）
    patterns.sort_by_key(|p| std::cmp::Reverse(p.parts.iter().map(|s| s.len()).sum::<usize>()));
    Dict { exact, patterns }
}

pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == 1 {
        Lang::En
    } else {
        Lang::Zh
    }
}

pub fn set_lang(l: Lang) {
    LANG.store(if l == Lang::En { 1 } else { 0 }, Ordering::Relaxed);
}

/// 设置里的取值：auto / zh / en
pub fn pref(store: &Store) -> String {
    store.get_kv("lang").filter(|v| matches!(v.as_str(), "zh" | "en" | "auto")).unwrap_or_else(|| "auto".into())
}

/// 按设置生效。自动化测试（EASYMAIL_SCRIPT）里 auto 固定为中文，保证断言稳定
pub fn apply(store: &Store) {
    // 调试 / 截图用：EASYMAIL_LANG=en|zh 直接指定
    match std::env::var("EASYMAIL_LANG").as_deref() {
        Ok("en") => return set_lang(Lang::En),
        Ok("zh") => return set_lang(Lang::Zh),
        _ => {}
    }
    let l = match pref(store).as_str() {
        "zh" => Lang::Zh,
        "en" => Lang::En,
        _ if std::env::var_os("EASYMAIL_SCRIPT").is_some() => Lang::Zh,
        _ => system_lang(),
    };
    set_lang(l);
}

fn system_lang() -> Lang {
    let out = std::process::Command::new("defaults").args(["read", "-g", "AppleLanguages"]).output().ok();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let first = text.split('"').nth(1).unwrap_or("");
    if first.is_empty() || first.starts_with("zh") {
        Lang::Zh
    } else {
        Lang::En
    }
}

fn translate(s: &str) -> Option<String> {
    let d = &*DICT;
    if let Some(en) = d.exact.get(s) {
        return Some(en.clone());
    }
    'pat: for p in &d.patterns {
        let first = &p.parts[0];
        let last = &p.parts[p.parts.len() - 1];
        if s.len() < first.len() + last.len() || !s.starts_with(first.as_str()) || !s.ends_with(last.as_str()) {
            continue;
        }
        let mut caps: Vec<&str> = Vec::new();
        let mut rest = &s[first.len()..s.len() - last.len()];
        for mid in &p.parts[1..p.parts.len() - 1] {
            let Some(i) = rest.find(mid.as_str()) else { continue 'pat };
            caps.push(&rest[..i]);
            rest = &rest[i + mid.len()..];
        }
        caps.push(rest);
        let mut out = String::new();
        let mut it = caps.into_iter();
        let mut pieces = p.en.split("{}").peekable();
        while let Some(piece) = pieces.next() {
            out.push_str(piece);
            if pieces.peek().is_some() {
                let c = it.next().unwrap_or("");
                out.push_str(&tr(c));
            }
        }
        return Some(out);
    }
    None
}

/// 翻译一句界面文案。中文界面、或词典里没有时原样返回
pub fn tr(s: &str) -> Cow<'_, str> {
    if lang() == Lang::Zh || !s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
        return Cow::Borrowed(s);
    }
    match translate(s) {
        Some(en) => Cow::Owned(en),
        None => Cow::Borrowed(s),
    }
}

/// 注册到 GPUI 的文字钩子：所有要画到界面上的字符串都先过一遍词典（见 vendor/PATCHES.md）
pub fn install_text_hook() {
    fn hook(s: &str) -> Option<String> {
        match tr(s) {
            Cow::Borrowed(_) => None,
            Cow::Owned(o) => Some(o),
        }
    }
    let _ = gpui::TEXT_HOOK.set(hook);
}

/// 给 GPUI 的 `SharedString` 用
pub fn tr_shared(s: gpui::SharedString) -> gpui::SharedString {
    match tr(&s) {
        Cow::Borrowed(_) => s,
        Cow::Owned(o) => o.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_pattern() {
        set_lang(Lang::En);
        assert_eq!(tr("保存"), "Save");
        assert_eq!(tr("已选 3"), "3 selected");
        assert_eq!(tr("已移动到「工作」"), "Moved to \"工作\"");
        // 捕获内容递归翻译
        assert_eq!(tr("同步中 · 刚刚"), "Syncing · Just now");
        // 词典里没有 → 原样
        assert_eq!(tr("没有这句话"), "没有这句话");
        // 不含中文不处理
        assert_eq!(tr("Inbox"), "Inbox");
        set_lang(Lang::Zh);
        assert_eq!(tr("保存"), "保存");
    }

    #[test]
    fn dictionary_is_well_formed() {
        let src = include_str!("../locales/en.tsv");
        let mut seen = std::collections::HashSet::new();
        for (n, line) in src.lines().enumerate() {
            let (zh, en) = line.split_once('\t').unwrap_or_else(|| panic!("第 {} 行缺少制表符", n + 1));
            assert!(!en.trim().is_empty(), "第 {} 行译文为空", n + 1);
            assert!(seen.insert(zh.to_string()), "第 {} 行重复：{zh}", n + 1);
            // 占位符个数要一致，否则代入会错位
            assert_eq!(zh.matches("{}").count(), en.matches("{}").count(), "第 {} 行占位符个数不一致：{zh}", n + 1);
        }
    }
}
