//! 账号增删改查（数据库层）。连接测试在 imap.rs / smtp.rs。

use super::db::{now_ms, Store};
use super::model::Account;
use super::providers::{self, detect_provider, resolve_server, LOCAL_EMAIL, LOCAL_PROVIDER, PALETTE};
use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension};

pub fn list(store: &Store) -> Result<Vec<Account>> {
    let c = store.r();
    let mut st = c.prepare(&format!("SELECT {} FROM accounts ORDER BY sort, id", Account::COLS))?;
    let v = st.query_map([], Account::from_row)?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

pub fn get(store: &Store, id: i64) -> Result<Option<Account>> {
    Ok(store.r().query_row(&format!("SELECT {} FROM accounts WHERE id = ?", Account::COLS), [id], Account::from_row).optional()?)
}

pub fn must(store: &Store, id: i64) -> Result<Account> {
    match get(store, id)? {
        Some(a) => Ok(a),
        None => bail!("账号不存在"),
    }
}

/// 「本地归档」不是真实邮箱：导入进来的邮件挂在它下面，所有 IMAP 路径都必须跳过它。
pub fn ensure_local(store: &Store) -> Result<Account> {
    let existing =
        store.r().query_row(&format!("SELECT {} FROM accounts WHERE provider = ?", Account::COLS), [LOCAL_PROVIDER], Account::from_row).optional()?;
    if let Some(a) = existing {
        return Ok(a);
    }
    let secret = store.crypto().encrypt("");
    let id = {
        let c = store.w();
        c.execute(
            "INSERT INTO accounts (email, name, provider, imap_host, imap_port, smtp_host, smtp_port, smtp_secure, use_proxy, secret, color, sync_enabled, created_at, sort)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![LOCAL_EMAIL, "本地归档", LOCAL_PROVIDER, "", 0, "", 0, 0, 0, secret, "muted", 0, now_ms(), 9999],
        )?;
        c.last_insert_rowid()
    };
    must(store, id)
}

/// 新增 / 编辑账号表单的输入
#[derive(Clone, Debug, Default)]
pub struct AccountInput {
    pub email: String,
    pub password: String,
    pub name: String,
    pub provider: String,
    pub use_proxy: Option<bool>,
    pub signature: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
}

pub fn valid_email(email: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"^[^@\s]+@[^@\s]+\.[^@\s]+$").expect("regex"));
    let re = &*RE;
    re.is_match(email)
}

/// 构造一个「待测试」的账号（尚未入库）。
pub fn draft(input: &AccountInput, existing: Option<&Account>) -> Result<Account> {
    let email = if input.email.trim().is_empty() { existing.map(|e| e.email.clone()).unwrap_or_default() } else { input.email.trim().to_lowercase() };
    if !valid_email(&email) {
        bail!("邮箱地址格式不正确");
    }
    let provider = if !input.provider.is_empty() {
        input.provider.clone()
    } else if let Some(e) = existing {
        e.provider.clone()
    } else {
        detect_provider(&email).to_string()
    };
    let (ih, ip, sh, sp, ss) = match existing {
        Some(e) if input.imap_host.is_empty() => (e.imap_host.clone(), e.imap_port, e.smtp_host.clone(), e.smtp_port, e.smtp_secure),
        _ => {
            let imap = (!input.imap_host.is_empty()).then_some((input.imap_host.as_str(), if input.imap_port == 0 { 993 } else { input.imap_port }));
            let smtp = (!input.smtp_host.is_empty()).then_some((input.smtp_host.as_str(), if input.smtp_port == 0 { 465 } else { input.smtp_port }));
            let s = resolve_server(&email, &provider, imap, smtp);
            (s.imap_host, s.imap_port, s.smtp_host, s.smtp_port, s.smtp_secure)
        }
    };
    if ih.is_empty() || sh.is_empty() {
        bail!("请填写 IMAP / SMTP 服务器地址");
    }
    let use_proxy = input.use_proxy.unwrap_or_else(|| existing.map(|e| e.use_proxy).unwrap_or(providers::provider(&provider).needs_proxy));
    Ok(Account {
        id: existing.map(|e| e.id).unwrap_or(0),
        email,
        provider,
        imap_host: ih,
        imap_port: ip,
        smtp_host: sh,
        smtp_port: sp,
        smtp_secure: ss,
        use_proxy,
        secret: existing.map(|e| e.secret.clone()).unwrap_or_default(),
        sync_enabled: true,
        ..Default::default()
    })
}

/// 授权码 / 应用专用密码类的服务商：网页上显示成「abcd efgh ijkl mnop」，粘贴时常带空格，去掉不影响登录。
/// 自定义服务器的密码原样保留（真实密码里可能就有空格），只去掉首尾的换行。
pub fn clean_password(provider: &str, p: &str) -> String {
    if matches!(provider, "gmail" | "qq" | "netease163" | "icloud" | "yahoo" | "outlook" | "agentmail" | "aliyun" | "sina" | "sohu" | "china139") {
        p.chars().filter(|c| !c.is_whitespace()).collect()
    } else {
        p.trim_matches(['\r', '\n']).to_string()
    }
}

pub fn create(store: &Store, acc: &Account, password: &str, name: &str, signature: &str) -> Result<Account> {
    let exists: Option<i64> = store.r().query_row("SELECT id FROM accounts WHERE email = ?", [&acc.email], |r| r.get(0)).optional()?;
    if exists.is_some() {
        bail!("这个邮箱已经添加过了");
    }
    let n: i64 = store.r().query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))?;
    let display = if name.trim().is_empty() { acc.email.split('@').next().unwrap_or("").to_string() } else { name.trim().to_string() };
    let secret = store.crypto().encrypt(&clean_password(&acc.provider, password));
    let id = {
        let c = store.w();
        c.execute(
            "INSERT INTO accounts (email, name, provider, imap_host, imap_port, smtp_host, smtp_port, smtp_secure, use_proxy, secret, color, signature, created_at, sort)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                acc.email,
                display,
                acc.provider,
                acc.imap_host,
                acc.imap_port,
                acc.smtp_host,
                acc.smtp_port,
                acc.smtp_secure as i64,
                acc.use_proxy as i64,
                secret,
                PALETTE[(n as usize) % PALETTE.len()],
                signature,
                now_ms(),
                n
            ],
        )?;
        c.last_insert_rowid()
    };
    must(store, id)
}

#[derive(Clone, Debug, Default)]
pub struct AccountPatch {
    pub name: Option<String>,
    pub signature: Option<String>,
    pub use_proxy: Option<bool>,
    pub sync_enabled: Option<bool>,
    pub password: Option<String>,
}

/// 返回 (新账号, 连接参数是否变了)
pub fn update(store: &Store, id: i64, p: &AccountPatch) -> Result<(Account, bool)> {
    let a = must(store, id)?;
    // 多个字段要么全改、要么都不改
    store.tx(|c| {
        if let Some(n) = &p.name {
            let n = if n.trim().is_empty() { a.email.clone() } else { n.trim().to_string() };
            c.execute("UPDATE accounts SET name = ? WHERE id = ?", params![n, id])?;
        }
        if let Some(s) = &p.signature {
            c.execute("UPDATE accounts SET signature = ? WHERE id = ?", params![s, id])?;
        }
        if let Some(u) = p.use_proxy {
            c.execute("UPDATE accounts SET use_proxy = ? WHERE id = ?", params![u as i64, id])?;
        }
        if let Some(s) = p.sync_enabled {
            c.execute("UPDATE accounts SET sync_enabled = ? WHERE id = ?", params![s as i64, id])?;
        }
        if let Some(pw) = p.password.as_ref().filter(|p| !p.trim().is_empty()) {
            c.execute("UPDATE accounts SET secret = ? WHERE id = ?", params![store.crypto().encrypt(&clean_password(&a.provider, pw)), id])?;
        }
        c.execute("UPDATE accounts SET last_error = '' WHERE id = ?", [id])?;
        Ok(())
    })?;
    let changed = p.password.as_ref().is_some_and(|p| !p.trim().is_empty()) || p.use_proxy.is_some_and(|u| u != a.use_proxy);
    Ok((must(store, id)?, changed))
}

pub fn delete(store: &Store, id: i64) -> Result<()> {
    store.w().execute("DELETE FROM accounts WHERE id = ?", [id])?;
    let _ = std::fs::remove_dir_all(store.dir().join("eml").join(id.to_string()));
    Ok(())
}

pub fn set_error(store: &Store, id: i64, msg: &str) {
    let _ = store.w().execute("UPDATE accounts SET last_error = ? WHERE id = ?", params![msg, id]);
}

pub fn password(store: &Store, acc: &Account) -> Result<String> {
    if acc.is_oauth() {
        anyhow::bail!("该账号使用 OAuth 登录，没有密码");
    }
    Ok(clean_password(&acc.provider, &store.crypto().decrypt(&acc.secret)?))
}

/// 连接服务器用的凭据：密码账号返回密码；OAuth 账号返回（必要时刷新过的）access_token
pub async fn credential(store: &Store, acc: &Account, proxy: Option<&super::net::Proxy>) -> Result<String> {
    if acc.is_oauth() {
        let proxy = super::net::effective_proxy(acc.use_proxy, proxy)?;
        return super::oauth::access_token(store, acc, proxy).await;
    }
    password(store, acc)
}

/// 新建或更新一个 OAuth 账号（按邮箱地址认）；令牌加密存入 secret
pub fn save_oauth(store: &Store, d: &Account, token: &super::oauth::Token) -> Result<Account> {
    let secret = token.to_secret();
    let existing: Option<i64> = store.r().query_row("SELECT id FROM accounts WHERE email = ?", [&d.email], |r| r.get(0)).optional()?;
    let id = match existing {
        Some(id) => {
            store.w().execute(
                "UPDATE accounts SET secret = ?, auth = 'oauth2', provider = ?, imap_host = ?, imap_port = ?, smtp_host = ?, smtp_port = ?, smtp_secure = ?, last_error = '' WHERE id = ?",
                rusqlite::params![store.crypto().encrypt(&secret), d.provider, d.imap_host, d.imap_port as i64, d.smtp_host, d.smtp_port as i64, d.smtp_secure as i64, id],
            )?;
            id
        }
        None => {
            let a = create(store, d, "-", &d.name, &d.signature)?;
            // create 会按密码规则清洗，令牌直接覆盖写入
            store.w().execute("UPDATE accounts SET auth = 'oauth2', secret = ? WHERE id = ?", rusqlite::params![store.crypto().encrypt(&secret), a.id])?;
            a.id
        }
    };
    must(store, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crud() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let input = AccountInput { email: " Me@163.com ".into(), password: "ab cd".into(), ..Default::default() };
        let d = draft(&input, None).unwrap();
        assert_eq!(d.email, "me@163.com");
        assert_eq!(d.provider, "netease163");
        assert_eq!(d.imap_host, "imap.163.com");
        assert!(!d.use_proxy);
        let a = create(&s, &d, "ab cd", "", "").unwrap();
        assert_eq!(a.name, "me");
        assert_eq!(password(&s, &a).unwrap(), "abcd");
        assert!(create(&s, &d, "x", "", "").is_err(), "重复添加");
        let g = draft(&AccountInput { email: "x@gmail.com".into(), ..Default::default() }, None).unwrap();
        assert!(g.use_proxy, "Gmail 默认走代理");
        let (a2, changed) = update(&s, a.id, &AccountPatch { name: Some("大卫".into()), use_proxy: Some(true), ..Default::default() }).unwrap();
        assert_eq!(a2.name, "大卫");
        assert!(changed);
        let local = ensure_local(&s).unwrap();
        assert!(local.is_local());
        assert_eq!(ensure_local(&s).unwrap().id, local.id);
        assert_eq!(list(&s).unwrap().len(), 2);
        delete(&s, a.id).unwrap();
        assert_eq!(list(&s).unwrap().len(), 1);
        assert!(draft(&AccountInput { email: "bad".into(), ..Default::default() }, None).is_err());
        assert!(draft(&AccountInput { email: "a@corp.io".into(), provider: "custom".into(), ..Default::default() }, None).is_err());
    }

    #[test]
    fn password_cleaning_is_provider_specific() {
        assert_eq!(clean_password("gmail", "abcd efgh ijkl mnop"), "abcdefghijklmnop");
        assert_eq!(clean_password("custom", " pass word \n"), " pass word ", "自定义服务器的密码保留空格");
    }
}
