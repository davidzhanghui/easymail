# 参与贡献

感谢你的兴趣！提 PR 前请看一下下面几点。

## 开发环境

- macOS + Rust（版本由 `rust-toolchain.toml` 固定，`rustup` 自动安装）
- 端到端测试需要 Java 11+（GreenMail，不需要 docker）

```bash
cargo run                  # 开发运行
scripts/check.sh           # fmt → clippy -D warnings → 单元测试 → GreenMail 端到端 → 界面端到端
scripts/check.sh --no-ui   # 没有图形会话时跳过界面测试
```

提交前请确保 `scripts/check.sh --no-ui` 通过（CI 跑的就是它）。

## 约定

- **表结构**：只往 `src/core/db.rs` 的 `MIGRATIONS` 末尾追加，不改已有条目。
- **远端操作**：标记 / 移动 / 删除走 `pending_ops` 持久化队列（`src/core/pending.rs`），不要直接在 worker 上执行。
- **vendor/gpui**：尽量不改。必须改时同步更新 `vendor/PATCHES.md` 与 `vendor/gpui-0.2.2.patch`。
- 新增功能请带测试；不熟悉的领域先看看有没有成熟的开源实现再动手。
- 提交信息简明说明「改了什么、为什么」，中英文均可。

## 提交 issue

- Bug：请附 macOS 版本、EasyMail 版本、复现步骤；日志在 `~/Library/Application Support/EasyMail/data/easymail.log`（`EASYMAIL_LOG=debug` 可调高级别）。**贴日志前请抹掉邮箱地址与主机名等隐私信息。**
- 安全问题请走 [SECURITY.md](SECURITY.md)。

## 许可

提交贡献即表示你同意以 [MIT](LICENSE) 许可发布你的代码。
