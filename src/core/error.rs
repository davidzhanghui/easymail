//! 引擎边界上的错误分类：重试策略由类别决定。
//!
//! | 类别        | 例子                              | 处理                               |
//! |-------------|-----------------------------------|------------------------------------|
//! | Connection  | 超时、连接被重置、TLS 握手失败    | 重连后重试，按账号指数退避         |
//! | Auth        | 登录被拒                          | 停止自动重试，等用户改密码         |
//! | Rejected    | IMAP `NO` / `BAD`                 | 不重试，这条操作判定失败（回滚）   |
//!
//! 错误仍然以 `anyhow::Error` 传递（保留现有的中文上下文），分类信息挂在错误链里的 [`MailError`] 上，
//! 用 [`kind_of`] 取出；没有分类信息的错误按 Connection 处理（保守：可以重试）。

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Connection,
    Auth,
    Rejected,
}

#[derive(Debug)]
pub struct MailError {
    pub kind: ErrorKind,
    pub msg: String,
}

impl fmt::Display for MailError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for MailError {}

pub fn err(kind: ErrorKind, msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(MailError { kind, msg: msg.into() })
}

pub fn connection(msg: impl Into<String>) -> anyhow::Error {
    err(ErrorKind::Connection, msg)
}

pub fn auth(msg: impl Into<String>) -> anyhow::Error {
    err(ErrorKind::Auth, msg)
}

pub fn rejected(msg: impl Into<String>) -> anyhow::Error {
    err(ErrorKind::Rejected, msg)
}

/// 取错误链上第一个分类；没有就当连接类
pub fn kind_of(e: &anyhow::Error) -> ErrorKind {
    e.chain().find_map(|c| c.downcast_ref::<MailError>()).map(|m| m.kind).unwrap_or(ErrorKind::Connection)
}

/// async-imap 的错误：服务器明确回了 NO / BAD 是「拒绝」，其余（IO、断线、解析失败）都按连接问题处理
pub fn from_imap(what: &str, e: async_imap::error::Error) -> anyhow::Error {
    use async_imap::error::Error as E;
    let kind = match e {
        E::No(_) | E::Bad(_) => ErrorKind::Rejected,
        _ => ErrorKind::Connection,
    };
    err(kind, format!("{what}失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn kind_survives_context() {
        let e = Err::<(), _>(rejected("移动失败：no response: [TRYCREATE]")).context("外层说明").unwrap_err();
        assert_eq!(kind_of(&e), ErrorKind::Rejected);
        assert_eq!(kind_of(&anyhow::anyhow!("普通错误")), ErrorKind::Connection);
        assert_eq!(kind_of(&auth("登录被拒绝")), ErrorKind::Auth);
        let no = from_imap("移动", async_imap::error::Error::No("nope".into()));
        assert_eq!(kind_of(&no), ErrorKind::Rejected);
        assert!(no.to_string().starts_with("移动失败"));
        assert_eq!(kind_of(&from_imap("x", async_imap::error::Error::ConnectionLost)), ErrorKind::Connection);
    }
}
