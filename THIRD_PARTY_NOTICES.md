# 第三方许可声明

EasyMail 本身以 [MIT](LICENSE) 发布。仓库内包含以下第三方代码：

## vendor/gpui

- 来源：[gpui 0.2.2](https://crates.io/crates/gpui)（Zed Industries）
- 许可：Apache License 2.0，全文见 [`vendor/gpui/LICENSE-APACHE`](vendor/gpui/LICENSE-APACHE)
- 修改：见 [`vendor/PATCHES.md`](vendor/PATCHES.md) 与 [`vendor/gpui-0.2.2.patch`](vendor/gpui-0.2.2.patch)；被修改的文件均保留原版权声明。

## Cargo 依赖

其余依赖通过 crates.io 引入，各自许可见 `Cargo.lock` 中对应的 crate。生成完整清单：

```bash
cargo install cargo-license && cargo license
```

## 图标

`assets/icons/*.svg` 与 `assets/logo*` 为本项目自有素材，随仓库 MIT 许可发布。
