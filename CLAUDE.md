1. 用中文回复
2. 不熟悉的功能别从零写，先去Github或互联网上搜开源项目，对比思路再动手3. 常用命令：`cargo run` 开发运行；`scripts/check.sh` 全量检查（fmt → clippy -D warnings → 单元测试 → GreenMail 端到端 → 界面端到端）；`scripts/check.sh --no-ui` 跳过界面测试；`scripts/build.sh` 打包
4. 端到端测试依赖本机 Java + `tools/greenmail-standalone.jar`（`scripts/fetch-greenmail.sh` 下载）；设 `EASYMAIL_REQUIRE_GREENMAIL=1` 时缺依赖直接失败而不是跳过
5. 改表结构只往 `src/core/db.rs` 的 `MIGRATIONS` 末尾追加，不改已有条目，不改 just-mail 原有的列
6. 标记 / 移动 / 删除等远端操作走 `pending_ops` 持久化队列（`src/core/pending.rs`），不要再直接在 worker 上执行
