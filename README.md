<p align="center"><img src="assets/logo-256.png" width="128" alt="EasyMail"></p>

<h1 align="center">EasyMail · 迷你邮件</h1>

<p align="center">本地优先的多账号邮件客户端与归档工具 · Rust + <a href="https://www.gpui.rs/">GPUI</a> · macOS 原生</p>

<p align="center">
  <a href="https://github.com/davidzhanghui/easymail/releases/latest"><img src="https://img.shields.io/github/v/release/davidzhanghui/easymail?label=下载" alt="Release"></a>
  <a href="https://github.com/davidzhanghui/easymail/actions/workflows/ci.yml"><img src="https://github.com/davidzhanghui/easymail/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/macOS-12%2B-black" alt="macOS 12+">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT"></a>
</p>

<p align="center">简体中文 · <a href="README_EN.md">English</a> · <a href="https://davidzhanghui.github.io/easymail/">官网</a></p>

![主界面](docs/screenshots/02-reader.png)

## 功能

- **多账号**：Gmail、网易 163 / 126、QQ 邮箱、AgentMail，以及任意 IMAP / SMTP 服务器（密码或授权码登录）；统一收件箱
- **本地优先**：邮件信封与正文缓存在本机 SQLite + `.eml` 文件里，离线可读可搜；离线时做的删除 / 移动 / 标记会排队，联网后自动送达
- **实时收信**：IMAP IDLE（不支持时 45 秒轮询），菜单栏托盘、Dock 角标、系统通知
- **全文搜索**：中英文子串搜索（FTS5 trigram + 中文二元组索引），支持 `from:` `to:` `subject:` `is:unread` `is:starred` `has:attachment`
- **安全的正文显示**：WKWebView 禁用脚本，CSP 默认拦截远程图片（追踪像素），点「显示图片」才放行
- **富文本写信**：回复 / 全部回复 / 转发、签名、附件、内嵌图片、草稿
- **十套配色**：七套浅色 + 三套深色，可跟随系统
- **导入 / 导出**：导入 `.eml` / `.mbox` 到「本地归档」；导出 mbox、CSV、标准 maildir
- **代理**：SOCKS5 / HTTP（含认证），可按账号开启

| 写信 | 深色主题 |
| --- | --- |
| ![写信](docs/screenshots/03-compose.png) | ![深色主题](docs/screenshots/05-dark.png) |
| **配色主题** | **账号管理** |
| ![配色主题](docs/screenshots/04-theme.png) | ![账号管理](docs/screenshots/06-accounts.png) |

## 安装

从 [Releases](https://github.com/davidzhanghui/easymail/releases/latest) 下载 `EasyMail-<版本>.dmg`（通用二进制，Apple Silicon 与 Intel 通用，macOS 12+），把「迷你邮件.app」拖入「应用程序」。

应用暂未经 Apple 公证，首次打开若提示「已损坏，无法打开」，在终端执行一次：

```bash
xattr -cr /Applications/迷你邮件.app
```

## 构建与运行

需要 macOS 与 Rust（版本由 `rust-toolchain.toml` 固定，`rustup` 会自动安装）。

```bash
cargo run                     # 开发运行
scripts/restart.sh            # 退出旧实例 → debug 增量构建 → 以 .app 形式启动（-n 不构建，-f 前台看日志）
scripts/check.sh              # 质量检查：fmt → clippy → 单元测试 → GreenMail 端到端 → 界面端到端
scripts/check.sh --no-ui      # 跳过需要图形会话的界面测试
scripts/build.sh              # 打包 dist/迷你邮件.app 与 DMG（arm64 + x86_64 通用二进制）
scripts/screenshots.sh        # 用演示数据重新生成 docs/screenshots/
scripts/push-tag.sh           # 按 Cargo.toml 版本打 tag 并推送，触发 GitHub Actions 发布 Release
```

端到端测试用本机的 [GreenMail](https://greenmail-mail-test.github.io/greenmail/)（Java，不用 docker）提供真实的 IMAP / SMTP。
jar 不在仓库里，`scripts/fetch-greenmail.sh` 会按固定版本与校验和下载到 `tools/`（`check.sh` 会自动调用）。
需要 Java 11 及以上。

## 数据目录

默认 `~/Library/Application Support/EasyMail/data`，可用环境变量 `EASYMAIL_DATA_DIR` 覆盖。

| 文件 | 内容 |
| --- | --- |
| `just-mail.db` | SQLite 数据库（WAL）。文件名沿用前身 just-mail，便于直接打开旧存档 |
| `eml/<账号>/<邮件>.eml` | 邮件原文缓存 |
| `uploads/` | 写信时添加的附件（发出后清理） |
| `master.key` | 加密邮箱密码用的主密钥（0600）。**备份数据目录时它和数据库在一起，请妥善保管** |
| `easymail.log` | 运行日志（5 MB 轮转，`EASYMAIL_LOG=debug` 调高级别） |
| `panic.log` | 崩溃信息 |

### 与 just-mail 的兼容

- 首次启动时如果发现 just-mail 的数据目录，会自动迁移过来。
- 表结构与 just-mail 一致；EasyMail 新增的只有独立的表与索引（`pending_ops`、`folder_sync`、`messages_bigram` 等，版本号记在 `PRAGMA user_version`），不改动原有的列。
- 本地归档账号的地址仍是 `local@just-mail`。
- 新写入的邮件 `html` 列保留 `cid:` 引用，内嵌图片在显示时从 `.eml` 取（旧版本会把图片 base64 展开存进数据库）。

## 安全与隐私

- 没有任何遥测与统计。除了你配置的邮件服务器（及代理），以及你点「显示图片」主动放行的远程图片，不会访问其他网络地址。
- 邮箱密码用 AES-256-GCM 加密后存在本机数据库，密钥在 `master.key`（0600）。
- 发现安全问题请看 [SECURITY.md](SECURITY.md)，不要直接公开提 issue。

## 参与贡献

欢迎 issue 与 PR，请先阅读 [CONTRIBUTING.md](CONTRIBUTING.md) 与 [行为准则](CODE_OF_CONDUCT.md)。更新记录见 [CHANGELOG.md](CHANGELOG.md)。

## 文档

- [vendor/gpui 补丁清单](vendor/PATCHES.md)
- [第三方许可声明](THIRD_PARTY_NOTICES.md)

## 许可

[MIT](LICENSE)。`vendor/gpui` 为 Apache-2.0，见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
