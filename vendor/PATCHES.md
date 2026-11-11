# vendor/gpui 补丁清单

`vendor/gpui` 是 crates.io 上 `gpui 0.2.2` 的完整副本，加了下面这些补丁（通过 `Cargo.toml` 的 `[patch.crates-io]` 生效）。
完整差异见同目录的 [`gpui-0.2.2.patch`](./gpui-0.2.2.patch)（`diff -ruN` 生成，可直接 `patch -p0` 重放）。

源码里的补丁都带 `[easymail patch]` 注释，可以 `grep -rn "easymail patch" vendor/gpui` 定位。

## 升级 GPUI 时

1. 把新版本解包到临时目录，按下表逐条把补丁移植过去（或先试 `patch -p0 --dry-run < vendor/gpui-0.2.2.patch`）。
2. 重新生成 diff：`cd ~/.cargo/registry/src/*/ && diff -ruN gpui-<新版本> <仓库>/vendor/gpui > <仓库>/vendor/gpui-<新版本>.patch`（路径前缀按现有文件的写法改成相对路径）。
3. 更新本表；跑 `scripts/check.sh`（含界面端到端，会截图核对中文字形与侧栏底色）。

## 补丁列表

| 文件 | 改了什么 | 为什么 |
| --- | --- | --- |
| `src/platform/mac/text_system.rs` | 同一个 PostScript 名对应多份字体时按字形数挑同一份；用与 `rasterize_glyph` 相同字号的 CTFont 测量字形边界；打开字体平滑 | 否则部分中文会用另一份字体里的字形画成错字；字形边界与实际光栅化不一致导致裁切；笔画粗细与原生 AppKit 文字一致 |
| `src/text_system/line_wrapper.rs`、`src/text_system/line_layout.rs` | 中日韩避头尾：闭合标点不在行首、开标点不在行尾（含测试） | 中文排版规范 |
| `src/text_system/line.rs` | 配合对齐方式的行内偏移计算 | 与下面的 `TextAlign` 补丁配套 |
| `src/elements/text.rs` | 命中测试 / 光标定位按 `TextAlign`（居中 / 右对齐）的行偏移计算；`prepaint` 拿到 `window` 读取当前文字样式 | 上游只在绘制时应用对齐，居中文字的点击位置与光标会错位 |
| `src/style.rs`、`src/styled.rs`、`src/taffy.rs` | 网格列宽权重 `grid_col_weights`（fr，负数为固定像素） | 上游 `grid_cols` 只能等分列 |
| `src/platform/mac/window.rs` | 透明标题栏下由应用自己的标题区域发起拖动窗口；`EASYMAIL_FORCE_DRAW=1` 时窗口被遮挡也继续绘制 | 自绘标题栏需要能拖窗口；后者仅供自动化截图测试 |
| `src/platform/mac/window.rs`（`handle_key_event`） | `performKeyEquivalent:` 时若 GPUIView 不是第一响应者，不再调用自己的 `inputContext handleEvent:`，只跑 GPUI 快捷键回调 | 写信正文的 WKWebView 持有焦点时，方向键会被输入法当成 U+F700–F703 文字提交进正文，显示成方框 |
| `src/platform/mac/events.rs` | 触摸板捏合缩放转成带 ⌘ 的滚轮事件 | 与 ⌘ + 滚轮缩放共用一条处理路径 |
| `src/platform.rs`、`src/platform/mac/platform.rs`、`src/platform/linux/*.rs`、`src/platform/windows/util.rs` | 新增 `CursorStyle::Move` 及各平台映射 | 拖动类交互需要「移动」光标 |
