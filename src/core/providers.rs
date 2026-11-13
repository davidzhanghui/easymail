//! 邮箱服务商预设与文件夹角色识别。
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
        id: "outlook",
        name: "Outlook / 365",
        domains: &["outlook.com", "hotmail.com", "live.com", "msn.com", "outlook.cn", "hotmail.co.uk", "live.cn"],
        imap: ("outlook.office365.com", 993),
        smtp: ("smtp.office365.com", 587, false),
        auto_save_sent: true,
        needs_proxy: false,
        skip_roles: &[],
        password_label: "使用 Microsoft 登录",
        help: &[
            "Microsoft 已关闭 Outlook.com / Microsoft 365 的密码登录（基本认证），必须用 OAuth",
            "点下方「用 Microsoft 账号登录」，在浏览器里登录并同意授权，完成后自动回到 EasyMail",
            "公司 / 学校账号如果提示需要管理员同意，请联系 IT 管理员放行，或在设置里填写本组织自己的 Client ID",
            "令牌加密保存在本机，随时可以在 account.microsoft.com → 隐私 → 应用访问权限 里撤销",
        ],
        hosts_by_domain: &[],
        placeholder: "name@outlook.com",
        auth_hint: "Outlook / Microsoft 365 只支持 OAuth 登录：请在账号设置里点「用 Microsoft 账号登录」重新授权。",
    },
    Provider {
        id: "icloud",
        name: "iCloud",
        domains: &["icloud.com", "me.com", "mac.com"],
        imap: ("imap.mail.me.com", 993),
        smtp: ("smtp.mail.me.com", 587, false),
        auto_save_sent: true,
        needs_proxy: false,
        skip_roles: &[],
        password_label: "App 专用密码",
        help: &[
            "打开 account.apple.com → 登录与安全 → App 专用密码，生成一个",
            "这里填生成的 xxxx-xxxx-xxxx-xxxx，不是 Apple ID 密码",
            "登录名用 @icloud.com 地址（不是 Apple ID 的其他邮箱）",
        ],
        hosts_by_domain: &[],
        placeholder: "name@icloud.com",
        auth_hint: "iCloud 要用 account.apple.com 生成的「App 专用密码」，不能用 Apple ID 密码。",
    },
    Provider {
        id: "yahoo",
        name: "Yahoo",
        domains: &["yahoo.com", "ymail.com", "yahoo.co.jp", "yahoo.co.uk", "rocketmail.com"],
        imap: ("imap.mail.yahoo.com", 993),
        smtp: ("smtp.mail.yahoo.com", 465, true),
        auto_save_sent: true,
        needs_proxy: true,
        skip_roles: &[],
        password_label: "应用密码",
        help: &["Yahoo 账号安全 → 生成应用密码（Generate app password）", "这里填生成的 16 位应用密码"],
        hosts_by_domain: &[("yahoo.co.jp", "imap.mail.yahoo.co.jp", "smtp.mail.yahoo.co.jp")],
        placeholder: "name@yahoo.com",
        auth_hint: "Yahoo 要用账号安全页生成的「应用密码」。",
    },
    Provider {
        id: "fastmail",
        name: "Fastmail",
        domains: &["fastmail.com", "fastmail.fm", "fastmail.cn", "messagingengine.com"],
        imap: ("imap.fastmail.com", 993),
        smtp: ("smtp.fastmail.com", 465, true),
        auto_save_sent: false,
        needs_proxy: false,
        skip_roles: &[],
        password_label: "应用密码",
        help: &["Fastmail 设置 → 隐私与安全 → 集成 → 新建应用密码，权限选 IMAP + SMTP"],
        hosts_by_domain: &[],
        placeholder: "name@fastmail.com",
        auth_hint: "Fastmail 要用设置里生成的「应用密码」。",
    },
    Provider {
        id: "exmail",
        name: "腾讯企业邮",
        domains: &["exmail.qq.com"],
        imap: ("imap.exmail.qq.com", 993),
        smtp: ("smtp.exmail.qq.com", 465, true),
        auto_save_sent: true,
        needs_proxy: false,
        skip_roles: &[],
        password_label: "客户端专用密码",
        help: &[
            "企业邮网页版 → 设置 → 客户端设置：开启 IMAP/SMTP 服务",
            "开启了「安全登录」时要在 微信绑定 → 生成客户端专用密码，填在这里",
            "邮箱地址填公司域名的地址",
        ],
        hosts_by_domain: &[],
        placeholder: "name@company.com",
        auth_hint: "腾讯企业邮开启安全登录后要用「客户端专用密码」。",
    },
    Provider {
        id: "aliyun",
        name: "阿里邮箱",
        domains: &["aliyun.com", "alibaba-inc.com", "alimail.com"],
        imap: ("imap.aliyun.com", 993),
        smtp: ("smtp.aliyun.com", 465, true),
        auto_save_sent: false,
        needs_proxy: false,
        skip_roles: &[],
        password_label: "密码 / 三方客户端密码",
        help: &[
            "网页版 → 设置 → 账户 → POP3/SMTP 及 IMAP：开启 IMAP",
            "企业版（公司域名）服务器为 imap.qiye.aliyun.com / smtp.qiye.aliyun.com，请在下方改地址",
        ],
        hosts_by_domain: &[],
        placeholder: "name@aliyun.com",
        auth_hint: "请确认阿里邮箱已开启 IMAP；开启了二次验证时要用「三方客户端安全密码」。",
    },
    Provider {
        id: "netease_ym",
        name: "网易企业邮",
        domains: &["ym.163.com", "qiye.163.com"],
        imap: ("imaphz.qiye.163.com", 993),
        smtp: ("smtphz.qiye.163.com", 465, true),
        auto_save_sent: false,
        needs_proxy: false,
        skip_roles: &[],
        password_label: "客户端授权码",
        help: &["网易企业邮网页版 → 设置 → 客户端设置：开启 IMAP/SMTP，生成客户端授权码", "北方节点服务器为 imap.qiye.163.com / smtp.qiye.163.com"],
        hosts_by_domain: &[],
        placeholder: "name@company.com",
        auth_hint: "网易企业邮要用「客户端授权码」。",
    },
    Provider {
        id: "china139",
        name: "139 邮箱",
        domains: &["139.com"],
        imap: ("imap.139.com", 993),
        smtp: ("smtp.139.com", 465, true),
        auto_save_sent: false,
        needs_proxy: false,
        skip_roles: &[],
        password_label: "客户端授权码",
        help: &["139 邮箱网页版 → 设置 → 客户端设置：开启 IMAP/SMTP，按提示获取授权码"],
        hosts_by_domain: &[],
        placeholder: "13800000000@139.com",
        auth_hint: "139 邮箱要用「客户端授权码」。",
    },
    Provider {
        id: "sina",
        name: "新浪邮箱",
        domains: &["sina.com", "sina.cn", "vip.sina.com"],
        imap: ("imap.sina.com", 993),
        smtp: ("smtp.sina.com", 465, true),
        auto_save_sent: false,
        needs_proxy: false,
        skip_roles: &[],
        password_label: "客户端授权码",
        help: &["新浪邮箱网页版 → 设置 → 客户端 POP/IMAP/SMTP：开启 IMAP4/SMTP 服务并获取授权码"],
        hosts_by_domain: &[("sina.cn", "imap.sina.cn", "smtp.sina.cn"), ("vip.sina.com", "imap.vip.sina.com", "smtp.vip.sina.com")],
        placeholder: "name@sina.com",
        auth_hint: "新浪邮箱要用「客户端授权码」。",
    },
    Provider {
        id: "sohu",
        name: "搜狐邮箱",
        domains: &["sohu.com"],
        imap: ("imap.sohu.com", 993),
        smtp: ("smtp.sohu.com", 465, true),
        auto_save_sent: false,
        needs_proxy: false,
        skip_roles: &[],
        password_label: "独立密码",
        help: &["搜狐邮箱 → 选项 → 设置 → POP3/SMTP/IMAP：开启服务并设置独立密码"],
        hosts_by_domain: &[],
        placeholder: "name@sohu.com",
        auth_hint: "搜狐邮箱要用开启服务时设置的「独立密码」。",
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
pub const LOCAL_EMAIL: &str = "local@easymail";

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
    role_label(role).map(|s| crate::i18n::tr(s).into_owned()).unwrap_or_else(|| name.to_string())
}

/// 账号颜色（侧栏圆点），值是主题色名。
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

// ---------------- 自动配置（Thunderbird ISPDB / 域名自己的 autoconfig） ----------------

/// 自动配置的结果
#[derive(Clone, Debug, PartialEq)]
pub struct AutoConfig {
    pub server: Server,
    /// 配置来源（给用户看）
    pub source: String,
}

/// 解析 Thunderbird 的 config-v1.1.xml：取第一个 IMAP 和第一个 SMTP
pub fn parse_autoconfig(xml: &str, email: &str) -> Option<Server> {
    use regex::Regex;
    let block = |tag: &str, ty: &str| -> Option<String> {
        let re = Regex::new(&format!(r#"(?s)<{tag}[^>]*type="{ty}"[^>]*>(.*?)</{tag}>"#)).ok()?;
        re.captures(xml).map(|c| c[1].to_string())
    };
    let field = |b: &str, f: &str| -> String {
        Regex::new(&format!(r"(?s)<{f}>\s*(.*?)\s*</{f}>")).ok().and_then(|r| r.captures(b).map(|c| c[1].to_string())).unwrap_or_default()
    };
    let (local, domain) = email.rsplit_once('@').unwrap_or(("", ""));
    let fill = |h: String| h.replace("%EMAILDOMAIN%", domain).replace("%EMAILLOCALPART%", local).replace("%EMAILADDRESS%", email);
    let i = block("incomingServer", "imap")?;
    let o = block("outgoingServer", "smtp")?;
    let ip: u16 = field(&i, "port").parse().ok()?;
    let sp: u16 = field(&o, "port").parse().ok()?;
    let ih = fill(field(&i, "hostname"));
    let sh = fill(field(&o, "hostname"));
    if ih.is_empty() || sh.is_empty() {
        return None;
    }
    // IMAP 只接受 993（SSL）或 143（STARTTLS）；SMTP 465 隐式 TLS，其余 STARTTLS
    let isock = field(&i, "socketType").to_uppercase();
    if isock == "PLAIN" {
        return None;
    }
    let ssock = field(&o, "socketType").to_uppercase();
    Some(Server { imap_host: ih, imap_port: ip, smtp_host: sh, smtp_port: sp, smtp_secure: ssock == "SSL" || sp == 465 })
}

/// 查询自动配置：先问域名自己的 autoconfig，再问 Thunderbird 的公共数据库（只发送域名，不发送完整地址）
pub async fn autoconfig(email: &str, proxy: Option<&super::net::Proxy>) -> anyhow::Result<AutoConfig> {
    let domain = email.rsplit_once('@').map(|(_, d)| d.trim().to_lowercase()).unwrap_or_default();
    if domain.is_empty() || !domain.contains('.') {
        anyhow::bail!("邮箱地址不完整");
    }
    let urls = [
        (format!("https://autoconfig.{domain}/mail/config-v1.1.xml"), format!("{domain} 的自动配置")),
        (format!("https://autoconfig.thunderbird.net/v1.1/{domain}"), "Thunderbird 公共配置库".to_string()),
    ];
    for (u, src) in urls {
        if let Ok(r) = super::http::get(&u, proxy).await {
            if r.ok() {
                if let Some(server) = parse_autoconfig(&r.text(), email) {
                    return Ok(AutoConfig { server, source: src });
                }
            }
        }
    }
    anyhow::bail!("没有找到 {domain} 的自动配置，请手动填写服务器地址")
}

#[cfg(test)]
mod autoconfig_tests {
    use super::*;

    #[test]
    fn parses_ispdb_xml() {
        let xml = r#"<clientConfig><emailProvider id="x"><incomingServer type="pop3"><hostname>pop.x.com</hostname><port>995</port></incomingServer>
<incomingServer type="imap"><hostname>imap.%EMAILDOMAIN%</hostname><port>993</port><socketType>SSL</socketType></incomingServer>
<outgoingServer type="smtp"><hostname>smtp.x.com</hostname><port>587</port><socketType>STARTTLS</socketType></outgoingServer></emailProvider></clientConfig>"#;
        let s = parse_autoconfig(xml, "a@x.com").unwrap();
        assert_eq!(s, Server { imap_host: "imap.x.com".into(), imap_port: 993, smtp_host: "smtp.x.com".into(), smtp_port: 587, smtp_secure: false });
        assert!(parse_autoconfig("<x/>", "a@x.com").is_none());
    }

    #[test]
    fn presets_detected() {
        assert_eq!(detect_provider("a@outlook.com"), "outlook");
        assert_eq!(detect_provider("a@icloud.com"), "icloud");
        assert_eq!(detect_provider("a@139.com"), "china139");
        let s = resolve_server("a@sina.cn", "sina", None, None);
        assert_eq!(s.imap_host, "imap.sina.cn");
        let o = resolve_server("a@outlook.com", "outlook", None, None);
        assert_eq!((o.smtp_port, o.smtp_secure), (587, false));
    }
}
