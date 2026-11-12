# 邮件服务商的怪癖

EasyMail 在对接各家 IMAP / SMTP 时踩过的坑与对应处理。写给想做同类客户端的人，也方便提 issue 时对照。
代码里的对应位置见每条后的文件名；预设参数在 `src/core/providers.rs`。

## 网易（163 / 126 / yeah.net / 企业邮）

- **登录后必须先发 `ID` 命令**，否则 `SELECT` 报 `Unsafe Login`。EasyMail 只上报客户端名称与版本，不上报联系邮箱。（`imap.rs`）
- 需要在网页端开启 IMAP/SMTP 服务并使用**授权码**，登录密码不行；授权码里的空格会被去掉（仅对已知用授权码的服务商）。（`accounts.rs` `clean_password`）
- **SMTP 有时会自己往「已发送」存一份**：发信后先按 Message-ID 在「已发送」里查，查不到才 APPEND，避免出现两份；服务器落地偶尔慢半拍，查询前会稍等。（`mailer.rs`、`engine.rs`）
- 空闲的连接会被悄悄断开，复用旧连接时第一条命令就 `Broken pipe`：丢掉后用新连接重试一次。（`engine.rs`）
- 文件夹名是中文（「已发送」「已删除」「垃圾邮件」），没有 SPECIAL-USE 时按中文名识别角色。（`providers.rs`）

## QQ 邮箱 / 腾讯企业邮

- 同样用**授权码**登录。
- **SMTP 必定把副本存进「已发送」**，客户端再 APPEND 会被拒绝：对 QQ 直接跳过 APPEND。（`providers.rs`）
- 文件夹名经 modified UTF-7 编码的中文，需要正确解码。（`utf7.rs`）

## Gmail

- 「所有邮件」「重要」「已加星标」是**标签视图**，同一封信会在多个文件夹里各出现一次：默认不同步这几个。（`providers.rs` `skip_roles`）
- SMTP 必存「已发送」，不 APPEND。
- 应用专用密码需要先开启两步验证；OAuth 需要自己在 Google Cloud 创建「桌面应用」凭据（开源客户端不能内置 Google 的 client secret）。
- 标签多的账号文件夹数量大：优先用 `LIST-STATUS`（RFC 5819）一次拿全计数，未同步的文件夹降低 STATUS 频率。（`imap.rs`、`engine.rs`）
- 在中国大陆需要代理；首次启动会探测本机常见代理端口。

## Outlook.com / Microsoft 365

- **已不再接受基本认证**（用户名 + 密码），只能 OAuth2（XOAUTH2）。IMAP `outlook.office365.com:993`，SMTP `smtp.office365.com:587` STARTTLS。（`oauth.rs`）
- 需要的权限：`https://outlook.office.com/IMAP.AccessAsUser.All`、`https://outlook.office.com/SMTP.Send`、`offline_access`。组织可能禁用了 SMTP AUTH，此时只能收信。
- 桌面应用的重定向 URI 用 `http://localhost`，端口随机（Azure 对 loopback 地址忽略端口）。

## iCloud

- 需要在 Apple ID 页面生成**App 专用密码**；用户名用完整邮箱地址。SMTP `smtp.mail.me.com:587` STARTTLS。

## AgentMail

- **不能取 `ENVELOPE`**：它返回的 ENVELOPE 会让 async-imap 的解析器陷入死循环。EasyMail 一律改取 `BODY.PEEK[HEADER.FIELDS (…)]` 交给 mail-parser。（`imap.rs`、`parse.rs`）
- 一次取多封原文会超时，逐封取。（`engine.rs`）

## 通用

- 没有 `UIDPLUS` 时不做全文件夹 `EXPUNGE`，以免清掉别的客户端标记删除的信。
- 服务器 LIST 结果比本地少一半以上时不删本地文件夹（防止临时故障导致误删）。
- 按 Message-ID 搜索时对 `\` 与 `"` 转义，否则含反斜杠的 Message-ID 会让 SEARCH 被拒。
- 端口 143 走 STARTTLS（非回环地址必须升级成功，否则拒绝连接）；明文只允许回环地址。
