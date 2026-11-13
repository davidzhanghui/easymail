<p align="center"><img src="assets/logo-256.png" width="128" alt="EasyMail"></p>

<h1 align="center">EasyMail</h1>

<p align="center">A local-first, multi-account email client and archiver · Rust + <a href="https://www.gpui.rs/">GPUI</a> · native macOS</p>

<p align="center">
  <a href="https://github.com/davidzhanghui/easymail/releases/latest"><img src="https://img.shields.io/github/v/release/davidzhanghui/easymail?label=download" alt="Release"></a>
  <a href="https://github.com/davidzhanghui/easymail/actions/workflows/ci.yml"><img src="https://github.com/davidzhanghui/easymail/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/macOS-12%2B-black" alt="macOS 12+">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT"></a>
</p>

<p align="center"><a href="README.md">简体中文</a> · English · <a href="https://davidzhanghui.github.io/easymail/">Website</a></p>

![Main window](docs/screenshots/02-reader.png)

> The UI is currently in Simplified Chinese only (the app is named 迷你邮件, "Mini Mail"). English localization is welcome as a contribution.

## Features

- **Multiple accounts** — Gmail, Outlook / Microsoft 365, iCloud, Yahoo, Fastmail, NetEase 163 / 126 / enterprise, QQ / Tencent Exmail, Aliyun Mail, 139, Sina, Sohu, AgentMail and any IMAP / SMTP server, with a unified inbox
- **OAuth sign-in** — Outlook / Microsoft 365 via browser authorization (XOAUTH2), no app password needed; Gmail too (bring your own Google client ID). Custom servers can be auto-configured from the email domain (Thunderbird ISPDB)
- **Local-first** — envelopes and bodies are cached in SQLite + `.eml` files, readable and searchable offline. Deletes / moves / flags made offline are queued and delivered when you're back online
- **Undo & unsend** — moves / deletes can be undone for a few seconds (⌘Z); outgoing mail goes through an outbox with a 5-second unsend window by default, scheduled sending, and automatic backoff on network errors
- **Real-time** — IMAP IDLE / NOTIFY; per-folder watching when the server has no NOTIFY (45 s polling fallback), menu-bar tray, Dock badge, system notifications
- **Full-text search** — substring search for CJK and Latin text (FTS5 trigram + CJK bigram index) with `from:` `to:` `subject:` `filename:` `in:` `is:unread` `is:starred` `has:attachment` `before:` `after:` `larger:` `smaller:` and `-exclude`; save frequent searches to the sidebar
- **Conversation view** — groups messages by References / In-Reply-To, one row per thread
- **Organizing** — local rules (auto move / mark / archive new mail), snooze, a per-sender view with bulk read / unsubscribe / delete, one-click unsubscribe (RFC 8058), an invoices view, reply templates and a ⌘K command palette
- **Privacy & safety** — WKWebView with JavaScript disabled and a non-persistent data store; a CSP blocks remote images by default (allow per sender); blocked tracking pixels and a privacy report; warnings when SPF / DKIM / DMARC fail or a link's text doesn't match its target; tracking parameters (utm_* etc.) are stripped when opening links
- **Reading helpers** — verification codes detected with one-click copy; flight / train / hotel itineraries
- **Rich-text composer in its own window** — reply / reply all / forward, signatures, attachments, inline images; drafts autosave and sync to the server's Drafts folder
- **Ten color themes** — seven light, three dark, or follow the system
- **Import / export / backup** — import `.eml` / `.mbox` into a local archive; export mbox, CSV or a standard maildir; verified full backups, scheduled incremental export, copy folders between accounts (resumable IMAP copy), and safe cache slimming
- **CLI & AI access** — `easymail-cli` searches, reads, and exports the local library, and can serve a read-only MCP server plus a localhost HTTP bridge. Off by default; turn it on under Tools → Local access, optionally limited to chosen accounts and folders
- **Proxies** — SOCKS5 / HTTP (with auth), per account; common local proxy ports are detected on first launch
- **Connection diagnostics** — step-by-step check of proxy → DNS → port → TLS → login

| Composer | Dark theme |
| --- | --- |
| ![Composer](docs/screenshots/03-compose.png) | ![Dark](docs/screenshots/05-dark.png) |
| **Themes** | **Accounts** |
| ![Themes](docs/screenshots/04-theme.png) | ![Accounts](docs/screenshots/06-accounts.png) |

## Install

Download `EasyMail-<version>.dmg` from [Releases](https://github.com/davidzhanghui/easymail/releases/latest) (universal binary for Apple Silicon and Intel, macOS 12+) and drag `迷你邮件.app` into Applications.

The app is not notarized yet. If macOS says it is "damaged", run once in Terminal:

```bash
xattr -cr /Applications/迷你邮件.app
```

## Build & run

Requires macOS and Rust (pinned by `rust-toolchain.toml`; `rustup` installs it automatically).

```bash
cargo run                     # run in development
scripts/restart.sh            # quit running instance → incremental debug build → launch as .app (-n: no build, -f: foreground logs)
scripts/check.sh              # fmt → clippy → unit tests → GreenMail end-to-end → UI end-to-end
scripts/check.sh --no-ui      # skip UI tests that need a graphical session
scripts/build.sh              # package dist/迷你邮件.app and a DMG (arm64 + x86_64 universal binary)
scripts/screenshots.sh        # regenerate docs/screenshots/ from demo data
scripts/bench.sh [count]      # large-library benchmark (100k synthetic messages by default, temp dir)
scripts/push-tag.sh           # tag the Cargo.toml version and push it, triggering the Release workflow
```

End-to-end tests run against a local [GreenMail](https://greenmail-mail-test.github.io/greenmail/) (Java, no Docker) for real IMAP / SMTP.
The jar is not committed; `scripts/fetch-greenmail.sh` downloads a pinned version with checksum into `tools/` (`check.sh` calls it). Requires Java 11+.

Parser fuzz targets live in `fuzz/` (`cargo +nightly fuzz run parse_message`, requires `cargo install cargo-fuzz`); on stable, `cargo test` also runs a randomized mutation test.

### Performance

`scripts/bench.sh` on Apple Silicon with 100,000 messages (median):

| Scenario | Time |
| --- | --- |
| Inbox first page, 60 rows (with count) | ~1 ms |
| Next page | ~2 ms |
| Full-text search (CJK / Latin / combined) | 22–28 ms |
| Conversation view first page | ~160 ms |
| Top 200 senders | ~95 ms |

## CLI & MCP

The packaged `迷你邮件.app/Contents/MacOS/easymail-cli` reads the local library directly (read-only; the app doesn't need to be running):

```bash
easymail-cli search "from:bob invoice after:2025-01" --limit 20
easymail-cli show 1234
easymail-cli attachments 1234
easymail-cli attachment 1234 0
easymail-cli export mbox ~/out.mbox "is:starred"
easymail-cli backup ~/Backups
easymail-cli mcp          # run the MCP server over stdio
```

Reading mail (including MCP and the local HTTP bridge) is off by default. Turn on the master switch under Tools → Local access, and optionally limit it to specific accounts and folders. Export and full backup do not use this switch.

MCP client configuration (tools: `search_mail` / `read_mail` / `list_attachments` / `get_attachment` / `list_folders` / `mail_stats`, all read-only — it cannot send or delete mail; message bodies and attachments are marked as untrusted content):

```json
{
  "mcpServers": {
    "easymail": {
      "command": "/Applications/迷你邮件.app/Contents/MacOS/easymail-cli",
      "args": ["mcp"]
    }
  }
}
```

Every tool call is logged to `mcp-audit.log`. The same screen can show the last 200 lines and clear them. The same read-only tools are also available over HTTP on `127.0.0.1:47321` (off by default, Bearer token required, and the master switch must be on). Turning this on allows authorized local programs to read mail in the selected scope.

## Data directory

Defaults to `~/Library/Application Support/EasyMail/data`; override with `EASYMAIL_DATA_DIR`.

| File | Contents |
| --- | --- |
| `easymail.db` | SQLite database (WAL) |
| `eml/<account>/<message>.eml` | Raw message cache |
| `uploads/` | Attachments added while composing (cleaned after sending) |
| `master.key` | Master key for encrypting mailbox passwords (0600). **Keep it safe when backing up the data directory** |
| `easymail.log` | Log (rotated at 5 MB; `EASYMAIL_LOG=debug` for more detail) |
| `panic.log` | Crash information (reported on next launch) |
| `mcp-audit.log` | Log of MCP / CLI tool calls |

## Security & privacy

- No telemetry. The app only contacts:
  - your mail servers (and proxy);
  - remote images you allow (per message or per sender);
  - update check: at most once a day, the public GitHub Releases API, with no device identifier; can be turned off in settings;
  - only when you act: OAuth sign-in (Microsoft / Google authorization servers), server auto-configuration (the domain's own autoconfig and the Thunderbird ISPDB), and one-click unsubscribe (the sender's unsubscribe URL).
- Passwords / OAuth tokens are encrypted with AES-256-GCM in the local database; the key lives in `master.key` (0600). Full backups include `master.key` — guard the backup like a password.
- Message bodies (`.eml` files and the database) are not encrypted at rest beyond FileVault; enabling FileVault is recommended.
- Please report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

### Outlook / Microsoft 365 OAuth

The built-in client ID is registered in Azure as a "Mobile and desktop application" with redirect URI `http://localhost`, multi-tenant plus personal Microsoft accounts. If your organization requires its own registration, enter your client ID in settings (it needs `IMAP.AccessAsUser.All`, `SMTP.Send` and `offline_access`). Gmail OAuth needs your own Google Cloud "Desktop app" credentials; without them, keep using an app password.

### Known limitations

- The UI is Simplified Chinese only.
- The GPUI-drawn UI has limited VoiceOver / accessibility support (GPUI doesn't expose a full accessibility tree yet).
- The app is not notarized, and the master key lives in the data directory rather than the Keychain.

## Contributing

Issues and PRs are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md). Release notes: [CHANGELOG.md](CHANGELOG.md).

## Docs

- [Provider quirks](PROVIDER_QUIRKS.md) (Chinese)
- [vendor/gpui patches](vendor/PATCHES.md)

## License

[MIT](LICENSE). `vendor/gpui` is Apache-2.0, see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
