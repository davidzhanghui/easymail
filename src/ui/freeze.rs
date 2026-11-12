//! WebView 冻结：原生 WKWebView 永远盖在 GPUI 画的内容之上，弹层 / 模态打开时会被它挡住。
//! 做法是先给 WebView 截一张图，用图片顶替原生视图（`set_frozen(true)` 把原生视图挪开），弹层关闭后再解冻。
//!
//! 以前这段状态（快照、是否正在截图）散落在 MailApp 里，写信窗口等别的地方也想要同样的行为时只能复制一遍；
//! 现在收拢成一个小状态机：调用方每次渲染前告诉它「现在需不需要冻结」，拿 `snapshot()` 画图即可。

use crate::webview;
use gpui::{AsyncApp, Image, ImageFormat};
use std::sync::Arc;

#[derive(Default)]
pub struct Freeze {
    snap: Option<Arc<Image>>,
    snapping: bool,
}

pub enum Step {
    /// 什么都不用做
    Idle,
    /// 需要截图：调用方 spawn 一个任务执行 [`Freeze::capture`]，完成后调 [`Freeze::finish`]
    Capture,
}

impl Freeze {
    /// 同步期望状态。返回 `Step::Capture` 时调用方需要异步截图
    pub fn sync(&mut self, want: bool) -> Step {
        if !want {
            if webview::is_frozen() {
                webview::set_frozen(false);
            }
            self.snap = None;
            return Step::Idle;
        }
        if !webview::is_live() || webview::is_frozen() || self.snapping {
            return Step::Idle;
        }
        self.snapping = true;
        Step::Capture
    }

    pub async fn capture(cx: &mut AsyncApp) -> Option<Vec<u8>> {
        webview::snapshot(cx).await
    }

    /// 截图完成：如果此时仍需要冻结，换上快照并挪开原生视图
    pub fn finish(&mut self, png: Option<Vec<u8>>, still_wanted: bool) {
        self.snapping = false;
        if still_wanted {
            self.snap = png.map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b)));
            webview::set_frozen(true);
        }
    }

    /// 冻结期间用来顶替 WebView 的快照
    pub fn snapshot(&self) -> Option<Arc<Image>> {
        self.snap.clone()
    }
}
