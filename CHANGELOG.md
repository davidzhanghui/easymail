# 更新记录

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [SemVer](https://semver.org/lang/zh-CN/)。

## [Unreleased]

## [1.0.1]

### 新增
- Outlook / Microsoft 365 OAuth 登录（XOAUTH2）；Gmail OAuth（需自备 Google Client ID）；自定义服务器按域名自动配置；新增 iCloud、Yahoo、Fastmail、腾讯企业邮、阿里邮箱、网易企业邮、139、新浪、搜狐预设；端口 143 支持 STARTTLS。
- 移动 / 删除可撤销（⌘Z）；发件箱：撤回发送、定时发送、失败退避重试。
- 会话视图、保存的搜索、扩展搜索语法（`before:` `after:` `larger:` `smaller:` `filename:` `in:` `-排除`）、发票视图、稍后处理、本地规则、发件人视图、一键退订（RFC 8058）、回复模板、⌘K 命令面板。
- 阅读区：验证码识别与复制、行程卡片、SPF / DKIM / DMARC 未通过警告、可疑链接提示、追踪像素统计与隐私报告、按发件人放行远程图片；打开链接时去掉跟踪参数。
- 草稿自动保存并同步到服务器「草稿」文件夹；关闭写信窗口自动存草稿。
- 邮箱账号支持新建 / 重命名 / 删除文件夹（直接在服务器上执行）。
- 完整备份（附校验报告）、定时增量导出、账号间搬家（可断点续传）、安全瘦身、连接诊断。
- `easymail-cli` 命令行与只读 MCP 服务器（调用记录写入 `mcp-audit.log`）。
- 应用内自动更新：发现新版本后可一键下载、用内置公钥校验 Ed25519 签名与 SHA-256、替换并重启（发布流水线需配置 `UPDATE_SIGNING_KEY`）；检查更新（可关闭）、首次启动自动探测本机代理、上次异常退出提示、记住分栏宽度。
- 系统通知改用 UNUserNotificationCenter。

### 改进
- OAuth 登录被服务器拒绝时给出更明确的原因（例如令牌有效但邮箱未启用 IMAP），并附上令牌的 aud / scp / tid 摘要（不含令牌本身）便于排查。
- 「更多设置」并入「账号与设置 → 设置」并分组；`scripts/build.sh` 优先用本机自签名证书 `EasyMail Dev` 签名，固定签名身份。
- 列表翻页不再重复计数，计数上限 10,000（显示「10,000+」）；正文预取按字节分批；未同步文件夹的 STATUS 轮询降频，支持 LIST-STATUS。
- 正文 HTML 不再重复存进数据库（从 .eml 读取）；内嵌图片 cid 精确替换。
- SMTP 写入 / DATA 超时，5xx 归为「被拒绝」不再重试。

### 安全
- rusqlite 升级到 0.42（内置 SQLite 修复 RUSTSEC-2026-0459）；CI 新增每周 `cargo audit`；解析器 fuzz 目标。
- 远程内容的 CSP 收紧为只放行图片；WKWebView 使用非持久化数据存储。

## [1.0.0]

- 首个公开版本：多账号 IMAP / SMTP、本地优先缓存与离线操作队列、IMAP IDLE 实时收信、中英文全文搜索、安全的 HTML 正文渲染、`.eml` / `.mbox` 导入与 mbox / CSV / maildir 导出、SOCKS5 / HTTP 代理。
- macOS 12+，通用二进制，Apple Silicon 与 Intel Mac 都能用。
- 暗色主题下回复 / 转发时，引用的原邮件改用主题色显示，不再出现深色字压在深色背景上、表头白底的问题（发出的邮件仍保留原样式）。
