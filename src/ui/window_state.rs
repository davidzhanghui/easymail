//! 窗口位置 / 大小持久化。kv `window` = "x,y,宽,高,最大化(0/1)"。
//!
//! GPUI 没有「窗口尺寸变化」的回调，但窗口移动 / 缩放都会触发一次重绘：`MailApp` 在渲染时比较
//! `window.window_bounds()`，变了就防抖保存（见 `MailApp::track_window`）；关窗时再保存一次。
//! 恢复时用当前的显示器布局校验，拔掉外接显示器后窗口不会跑到看不见的地方。

use crate::core::db::Store;
use gpui::*;

pub const MIN_W: f32 = 1000.;
pub const MIN_H: f32 = 600.;
pub const DEFAULT_W: f32 = 1400.;
pub const DEFAULT_H: f32 = 880.;
const KEY: &str = "window";

/// 位置 / 大小（逻辑像素）。不直接用 `Bounds<Pixels>`，方便序列化与测试
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn from_bounds(b: Bounds<Pixels>) -> Rect {
        Rect { x: f32::from(b.origin.x), y: f32::from(b.origin.y), w: f32::from(b.size.width), h: f32::from(b.size.height) }
    }

    pub fn to_bounds(self) -> Bounds<Pixels> {
        Bounds::new(point(px(self.x), px(self.y)), size(px(self.w), px(self.h)))
    }
}

pub fn encode(r: Rect, maximized: bool) -> String {
    format!("{:.0},{:.0},{:.0},{:.0},{}", r.x, r.y, r.w, r.h, if maximized { 1 } else { 0 })
}

pub fn decode(s: &str) -> Option<(Rect, bool)> {
    let v: Vec<f32> = s.split(',').map(|x| x.trim().parse::<f32>().ok()).collect::<Option<_>>()?;
    if v.len() != 5 || v.iter().any(|x| !x.is_finite()) {
        return None;
    }
    Some((Rect { x: v[0], y: v[1], w: v[2], h: v[3] }, v[4] == 1.))
}

/// 用当前的显示器布局校验并修正窗口矩形；放不进任何显示器则返回 None（用默认位置）。
/// - 至少 160×100 的区域落在某个显示器内，并且标题栏还能被抓到；
/// - 尺寸不小于最小窗口，也不超过该显示器。
pub fn fit(r: Rect, displays: &[Rect]) -> Option<Rect> {
    if r.w < 100. || r.h < 100. {
        return None;
    }
    displays.iter().find_map(|d| {
        let ow = (r.x + r.w).min(d.x + d.w) - r.x.max(d.x);
        let oh = (r.y + r.h).min(d.y + d.h) - r.y.max(d.y);
        let title_reachable = r.y >= d.y - 4. && r.y <= d.y + d.h - 60.;
        (ow >= 160. && oh >= 100. && title_reachable).then(|| Rect {
            x: r.x,
            y: r.y,
            w: r.w.max(MIN_W).min(d.w.max(MIN_W)),
            h: r.h.max(MIN_H).min(d.h.max(MIN_H)),
        })
    })
}

/// 启动时读上次的窗口状态（已用当前显示器校验）
pub fn load(store: &Store, cx: &App) -> Option<WindowBounds> {
    let (r, maximized) = decode(&store.get_kv(KEY)?)?;
    let displays: Vec<Rect> = cx.displays().iter().map(|d| Rect::from_bounds(d.bounds())).collect();
    let b = fit(r, &displays)?.to_bounds();
    Some(if maximized { WindowBounds::Maximized(b) } else { WindowBounds::Windowed(b) })
}

/// 把当前窗口状态存下来。全屏状态不存（保留上一次的窗口大小）
pub fn save(store: &Store, wb: WindowBounds) {
    let v = match wb {
        WindowBounds::Windowed(b) => encode(Rect::from_bounds(b), false),
        WindowBounds::Maximized(b) => encode(Rect::from_bounds(b), true),
        WindowBounds::Fullscreen(_) => return,
    };
    store.set_kv(KEY, &v);
}

#[cfg(test)]
mod tests {
    use super::{decode, encode, fit, Rect, MIN_H, MIN_W};

    const SCREEN: Rect = Rect { x: 0., y: 0., w: 1728., h: 1117. };
    const EXTERNAL: Rect = Rect { x: 1728., y: -200., w: 2560., h: 1440. };

    #[test]
    fn roundtrip() {
        let r = Rect { x: 120., y: 40., w: 1400., h: 880. };
        assert_eq!(decode(&encode(r, true)), Some((r, true)));
        assert_eq!(decode(&encode(r, false)), Some((r, false)));
        assert_eq!(decode("1,2,3"), None);
        assert_eq!(decode("a,b,c,d,e"), None);
        assert_eq!(decode("NaN,0,1400,880,0"), None);
    }

    #[test]
    fn keeps_a_window_that_fits() {
        let r = Rect { x: 100., y: 50., w: 1400., h: 880. };
        assert_eq!(fit(r, &[SCREEN]), Some(r));
    }

    #[test]
    fn rejects_window_on_a_removed_display() {
        // 上次在外接显示器上（x 在 1728 之后），现在只剩内置屏
        let r = Rect { x: 2000., y: 100., w: 1400., h: 880. };
        assert_eq!(fit(r, &[SCREEN]), None);
        assert_eq!(fit(r, &[SCREEN, EXTERNAL]), Some(r));
    }

    #[test]
    fn rejects_window_whose_titlebar_is_unreachable() {
        // 标题栏在屏幕上方之外 / 屏幕底部之下，没法拖回来
        assert_eq!(fit(Rect { x: 0., y: -300., w: 1400., h: 880. }, &[SCREEN]), None);
        assert_eq!(fit(Rect { x: 0., y: 1100., w: 1400., h: 880. }, &[SCREEN]), None);
    }

    #[test]
    fn clamps_size() {
        let small = fit(Rect { x: 0., y: 0., w: 400., h: 300. }, &[SCREEN]).unwrap();
        assert_eq!((small.w, small.h), (MIN_W, MIN_H));
        let big = fit(Rect { x: 0., y: 0., w: 5000., h: 4000. }, &[SCREEN]).unwrap();
        assert_eq!((big.w, big.h), (SCREEN.w, SCREEN.h));
    }
}
