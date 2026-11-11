//! 邮箱服务商预设与文件夹角色识别（与 just-mail 的 providers.mjs 一致）。
//!
//!   auto_save_sent  SMTP 发出后服务商会自动存入「已发送」→ 不要再 APPEND，否则出现两份
//!   skip_roles      不同步的文件夹角色（Gmail 的「所有邮件/重要/星标」是标签视图，会把同一封信重复拉几遍）
//!   needs_proxy     国内网络默认需要走代理

pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    pub domains: &'static [&'static str],
    pub imap: (&'static str, u16),
    pub smtp: (&'static str, u16, bool),
    pub auto_save_sent: bool,
    pub needs_proxy: bool,
    pub skip_roles: &'static [&'static str],
    pub password_label: &'static str,
    pub help: &'static [&'static str],
    pub hosts_by_domain: &'static [(&'static str, &'static str, &'static str)],
    pub placeholder: &'static str,
    pub auth_hint: &'static str,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "gmail",
        name: "Gmail",
        domains: &["gmail.com", "googlemail.com"],
        imap: ("imap.gmail.com", 993),
        smtp: ("smtp.gmail.com", 465, true),
        auto_save_sent: true,
        needs_proxy: true,
        skip_roles: &["all", "important", "flagged"],
        password_label: "应用专用密码",
        help: &[
            "登录 Google 账号 → 安全性 → 开启「两步验证」",
            "打开 myaccount.google.com/apppasswords 生成 16 位「应用专用密码」（空格可留可去）",
            "Gmail 设置 → 转发和 POP/IMAP → 确认 IMAP 已启用",
            "国内网络需勾选「通过代理连接」（默认 socks5://127.0.0.1:1080）",
        ],
        hosts_by_domain: &[],
        placeholder: "name@gmail.com",
        auth_hint: "Gmail 要用 16 位「应用专用密码」：先在 Google 账号里开启两步验证，再到 myaccount.google.com/apppasswords 生成，不能用 Google 账号密码。",
    },
    Provider {
        id: "netease163",
        name: "网易 163",
        domains: &["163.com", "126.com", "yeah.net"],
        imap: ("imap.163.com", 993),
        smtp: ("smtp.163.com", 465, true),
        // 163 的 SMTP 有时会自己存一份到「已发送」，发信后按 Message-ID 现查，避免存成两份
        auto_save_sent: false,
        needs_proxy: false,
        skip_roles: &[],
        password_label: "客户端授权码",
        help: &["网页版 163 邮箱 → 设置 → POP3/SMTP/IMAP", "开启「IMAP/SMTP 服务」，按提示新增一个「授权密码」", "这里填授权码，不是网页登录密码"],
        hosts_by_domain: &[("126.com", "imap.126.com", "smtp.126.com"), ("yeah.net", "imap.yeah.net", "smtp.yeah.net")],
        placeholder: "name@163.com",
        auth_hint: "163 要用网页版设置里生成的「客户端授权码」，不能用登录密码。",
    },
    Provider {
        id: "qq",
        name: "QQ 邮箱",
        domains: &["qq.com", "foxmail.com", "vip.qq.com"],
        imap: ("imap.qq.com", 993),
        smtp: ("smtp.qq.com", 465, true),
        // QQ 的 SMTP 自己会把副本存进「已发送」，再 APPEND 会被服务器拒绝
        auto_save_sent: true,
        needs_proxy: false,
        skip_roles: &[],
        password_label: "授权码（16 位）",
        help: &[
            "网页版 QQ 邮箱 → 右上角头像 → 设置 → 账号与安全 → 安全设置",
            "找到「POP3/IMAP/SMTP/Exchange/CardDAV/CalDAV 服务」，开启「IMAP/SMTP 服务」",
            "点「生成授权码」，按提示验证（QQ 密码或手机扫码），得到 16 位授权码",
            "这里填授权码，不是 QQ 登录密码；授权码只显示一次，重新生成后旧码立即失效",
            "发出的邮件由 QQ 服务端存入「已发送」；若在网页版关掉了「SMTP 发信后保存到服务器」，已发送里不会有副本",
        ],
        hosts_by_domain: &[],
        placeholder: "123456@qq.com",
        auth_hint: "QQ 要用网页版生成的 16 位「授权码」（设置 → 账号与安全 → 安全设置），不能用 QQ 登录密码。",
    },
    Provider {
        id: "agentmail",
        name: "AgentMail",
        domains: &["agentmail.to"],
        imap: ("imap.agentmail.to", 993),
        smtp: ("smtp.agentmail.to", 465, true),
        auto_save_sent: false,
        needs_proxy: false,
        skip_roles: &[],
        password_label: "API Key（am_...）",
        help: &[
            "console.agentmail.to → Dashboard → Inboxes：Inbox ID 列的邮箱地址就是登录用户名",
            "Dashboard → API Keys：创建一个 API Key（am_ 开头），作为这里的「密码」",
            "受限 Key 需要完整 read 权限组（inbox/message/label_*_read 等），否则 IMAP 能登录但收不到信",
            "IMAP 仅暴露 INBOX / Sent / Trash / Spam；Drafts 文件夹恒为空（草稿要走 API）",
            "附件上限 10MB，单封最多 50 个收件人",
        ],
        hosts_by_domain: &[],
        placeholder: "you@agentmail.to",
        auth_hint: "AgentMail 的「密码」是 API Key（am_ 开头），在 console.agentmail.to → API Keys 生成；受限 Key 需要完整 read 权限组，否则能登录但收不到信。",
    },
    Provider {
        id: "custom",
        name: "其他 IMAP",
        domains: &[],
        imap: ("", 993),
        smtp: ("", 465, true),
        auto_save_sent: false,
        needs_proxy: false,
        skip_roles: &[],
        password_label: "密码 / 授权码",
        help: &["填写服务商提供的 IMAP / SMTP 地址与端口（IMAP 需 SSL/TLS）"],
        hosts_by_domain: &[],
        placeholder: "name@example.com",
        auth_hint: "请确认服务商已开启 IMAP/SMTP，并填写客户端授权码而非登录密码。",
    },
];

pub const LOCAL_PROVIDER: &str = "local";
pub const LOCAL_EMAIL: &str = "local@just-mail";

pub fn provider(id: &str) -> &'static Provider {
    PROVIDERS.iter().find(|p| p.id == id).unwrap_or(&PROVIDERS[PROVIDERS.len() - 1])
}

pub fn detect_provider(email: &str) -> &'static str {
    let domain = email.rsplit_once('@').map(|(_, d)| d.to_lowercase()).unwrap_or_default();
    PROVIDERS.iter().find(|p| p.domains.contains(&domain.as_str())).map(|p| p.id).unwrap_or("custom")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Server {
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_secure: bool,
}

/// 按邮箱推断完整的连接参数；显式传入的字段优先。
pub fn resolve_server(email: &str, provider_id: &str, imap: Option<(&str, u16)>, smtp: Option<(&str, u16)>) -> Server {
    let p = provider(provider_id);
    let domain = email.rsplit_once('@').map(|(_, d)| d.to_lowercase()).unwrap_or_default();
    let by = p.hosts_by_domain.iter().find(|(d, _, _)| *d == domain);
    let (ih, ip) =
        imap.filter(|(h, _)| !h.is_empty()).map(|(h, p)| (h.to_string(), p)).unwrap_or_else(|| (by.map(|b| b.1).unwrap_or(p.imap.0).to_string(), p.imap.1));
    let (sh, sp) =
        smtp.filter(|(h, _)| !h.is_empty()).map(|(h, p)| (h.to_string(), p)).unwrap_or_else(|| (by.map(|b| b.2).unwrap_or(p.smtp.0).to_string(), p.smtp.1));
    // 465 = 隐式 TLS；587 / 25 = STARTTLS
    let secure = if smtp.is_some() { sp == 465 } else { p.smtp.2 };
    Server { imap_host: ih, imap_port: ip, smtp_host: sh, smtp_port: sp, smtp_secure: secure }
}

/// 文件夹角色：优先 SPECIAL-USE，其次按常见命名（163 用中文，Gmail 用 [Gmail]/…）。
pub fn folder_role(path: &str, leaf: &str, special: &[&str]) -> &'static str {
    if path.eq_ignore_ascii_case("INBOX") {
        return "inbox";
    }
    for f in special {
        let r = match f.to_ascii_lowercase().as_str() {
            "\\inbox" => "inbox",
            "\\sent" => "sent",
            "\\drafts" => "drafts",
            "\\trash" => "trash",
            "\\junk" => "junk",
            "\\archive" => "archive",
            "\\all" => "all",
            "\\flagged" => "flagged",
            "\\important" => "important",
            _ => "",
        };
        if !r.is_empty() {
            return r;
        }
    }
    let l = leaf.to_lowercase();
    let l = l.as_str();
    match l {
        "收件箱" => "inbox",
        "sent" | "sent items" | "sent messages" | "sent mail" | "已发送" | "已发邮件" => "sent",
        "draft" | "drafts" | "草稿" | "草稿箱" => "drafts",
        "trash" | "deleted" | "deleted items" | "deleted messages" | "已删除" | "垃圾箱" => "trash",
        "junk" | "spam" | "垃圾邮件" | "广告邮件" => "junk",
        "archive" | "归档" => "archive",
        _ => "",
    }
}

pub const ROLE_ORDER: [&str; 6] = ["inbox", "drafts", "sent", "archive", "junk", "trash"];

pub fn role_rank(role: &str) -> usize {
    ROLE_ORDER.iter().position(|r| *r == role).unwrap_or(99)
}

pub fn role_label(role: &str) -> Option<&'static str> {
    Some(match role {
        "inbox" => "收件箱",
        "drafts" => "草稿箱",
        "sent" => "已发送",
        "archive" => "归档",
        "junk" => "垃圾邮件",
        "trash" => "已删除",
        "all" => "所有邮件",
        "flagged" => "已加星标",
        "important" => "重要",
        _ => return None,
    })
}

pub fn folder_label(role: &str, name: &str) -> String {
    role_label(role).map(String::from).unwrap_or_else(|| name.to_string())
}

/// 账号颜色（侧栏圆点），与 just-mail 的 PALETTE 同名。
pub const PALETTE: [&str; 6] = ["accent", "gold", "danger", "live", "muted", "ink-2"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_and_resolve() {
        assert_eq!(detect_provider("x@Gmail.com"), "gmail");
        assert_eq!(detect_provider("x@126.com"), "netease163");
        assert_eq!(detect_provider("x@foxmail.com"), "qq");
        assert_eq!(detect_provider("x@corp.io"), "custom");
        let s = resolve_server("a@126.com", "netease163", None, None);
        assert_eq!(s.imap_host, "imap.126.com");
        assert_eq!(s.smtp_host, "smtp.126.com");
        assert!(s.smtp_secure);
        let c = resolve_server("a@corp.io", "custom", Some(("imap.corp.io", 993)), Some(("smtp.corp.io", 587)));
        assert_eq!((c.imap_host.as_str(), c.smtp_port, c.smtp_secure), ("imap.corp.io", 587, false));
    }

    #[test]
    fn roles() {
        assert_eq!(folder_role("INBOX", "INBOX", &[]), "inbox");
        assert_eq!(folder_role("[Gmail]/已发邮件", "已发邮件", &["\\Sent"]), "sent");
        assert_eq!(folder_role("草稿箱", "草稿箱", &[]), "drafts");
        assert_eq!(folder_role("Deleted Messages", "Deleted Messages", &[]), "trash");
        assert_eq!(folder_role("病毒文件夹", "病毒文件夹", &[]), "");
        assert_eq!(folder_role("[Gmail]/重要", "重要", &["\\Important"]), "important");
        assert_eq!(folder_label("sent", "Sent Messages"), "已发送");
        assert_eq!(folder_label("", "工作"), "工作");
    }
}
