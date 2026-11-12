//! 导入外部邮件（.eml / .mbox）到本地归档。

use super::icon;
use super::line_input::LineInput;
use super::theme::theme;
use super::widgets::*;
use crate::core::engine::Engine;
use crate::core::transfer::{self, ImportStat};
use futures::StreamExt;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::path::PathBuf;

pub enum ImportEvent {
    Close,
    Done { folder_id: i64 },
    Toast(String, bool),
}

impl EventEmitter<ImportEvent> for ImportModal {}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    Running { done: usize, percent: u8, imported: usize, skipped: usize },
    Done,
}

pub struct ImportModal {
    eng: Engine,
    folder: Entity<LineInput>,
    pub file: Option<PathBuf>,
    pub phase: Phase,
    pub result: Option<ImportStat>,
    pub error: String,
    drop_hover: bool,
}

pub fn importable(p: &std::path::Path) -> bool {
    matches!(p.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("eml" | "mbox" | "mbx" | "txt"))
}

impl ImportModal {
    pub fn new(eng: Engine, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let folder = cx.new(|cx| {
            let mut i = LineInput::new("文件夹名（不存在会自动创建）", cx);
            i.set_text(transfer::DEFAULT_FOLDER, cx);
            i.collapse_to_end(cx);
            i
        });
        ImportModal { eng, folder, file: None, phase: Phase::Idle, result: None, error: String::new(), drop_hover: false }
    }

    pub fn busy(&self) -> bool {
        matches!(self.phase, Phase::Running { .. })
    }

    fn choose(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let rx =
            cx.prompt_for_paths(PathPromptOptions {
                files: true, directories: false, multiple: false, prompt: Some("导入".into()), allowed_extensions: None
            });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if let Some(p) = rx.await.ok().and_then(|r| r.ok()).flatten().and_then(|v| v.into_iter().next()) {
                this.update(cx, |m, cx| m.pick(p, cx)).ok();
            }
        })
        .detach();
    }

    pub fn pick(&mut self, p: PathBuf, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        if !importable(&p) {
            self.error = "只支持 .eml（单封）与 .mbox（归档）文件".into();
            cx.notify();
            return;
        }
        self.file = Some(p.clone());
        self.run(p, cx);
    }

    fn run(&mut self, file: PathBuf, cx: &mut Context<Self>) {
        let folder = {
            let f = self.folder.read(cx).text().trim().to_string();
            if f.is_empty() {
                transfer::DEFAULT_FOLDER.to_string()
            } else {
                f
            }
        };
        self.error.clear();
        self.result = None;
        self.phase = Phase::Running { done: 0, percent: 0, imported: 0, skipped: 0 };
        cx.notify();
        let store = self.eng.store().clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<(usize, u8, usize, usize)>();
        let task = cx.background_spawn(async move {
            transfer::import_file(&store, &file, &folder, |st, done, pct| {
                let _ = tx.unbounded_send((done, pct, st.imported, st.skipped));
            })
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let progress = async {
                while let Some((done, percent, imported, skipped)) = rx.next().await {
                    let ok = this
                        .update(cx, |m, cx| {
                            if m.busy() {
                                m.phase = Phase::Running { done, percent, imported, skipped };
                                cx.notify();
                            }
                        })
                        .is_ok();
                    if !ok {
                        break;
                    }
                }
            };
            let (r, _) = futures::join!(task, progress);
            this.update(cx, |m, cx| {
                match r {
                    Ok(st) => {
                        m.phase = Phase::Done;
                        let fid = st.folder_id;
                        m.result = Some(st);
                        cx.emit(ImportEvent::Done { folder_id: fid });
                    }
                    Err(e) => {
                        m.phase = Phase::Idle;
                        m.error = e.to_string();
                        cx.emit(ImportEvent::Toast(format!("导入失败：{e}"), true));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for ImportModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let busy = self.busy();
        let close = disabled(button("imp-x", &t, Kind::Ghost, Some("close"), "", true), busy).on_click(cx.listener(|m, _, _, cx| {
            if !m.busy() {
                cx.emit(ImportEvent::Close)
            }
        }));
        let (t1, t2) = match &self.file {
            Some(f) => (
                f.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
                std::fs::metadata(f).map(|m| format!("{:.1} MB", m.len() as f64 / 1_048_576.)).unwrap_or_default(),
            ),
            None => ("点击选择，或把文件拖到这里".to_string(), "支持 .eml 单封、.mbox 归档（Thunderbird / Apple Mail / mutt 导出的都行）".to_string()),
        };
        let zone = div()
            .id("drop-zone")
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.))
            .py(px(26.))
            .rounded(px(10.))
            .border_1()
            .border_dashed()
            .border_color(if self.drop_hover { t.accent } else { t.line_2 })
            .when(self.drop_hover, |d| d.bg(t.accent_soft))
            .cursor_pointer()
            .child(icon("clip", 20., t.muted))
            .child(div().text_size(px(13.)).text_color(t.ink).child(t1))
            .child(div().text_size(px(11.5)).text_color(t.muted).child(t2))
            .on_click(cx.listener(|m, _, _, cx| m.choose(cx)))
            .on_drag_move::<ExternalPaths>(cx.listener(|m, ev: &DragMoveEvent<ExternalPaths>, _, cx| {
                let inside = ev.bounds.contains(&ev.event.position);
                if inside != m.drop_hover {
                    m.drop_hover = inside;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|m, paths: &ExternalPaths, _, cx| {
                m.drop_hover = false;
                if let Some(p) = paths.paths().first() {
                    m.pick(p.clone(), cx);
                }
            }));
        let focused = self.folder.read(cx).is_focused(window);
        let mut body = div()
            .px(px(22.))
            .py(px(16.))
            .child(section_title(&t, "选择文件"))
            .child(zone)
            .child(form_row(&t, "导入到", false).mt(px(14.)).child(field_box(&t, focused).flex_1().child(div().flex_1().child(self.folder.clone()))))
            .child(div().ml(px(96.)).text_size(px(11.5)).text_color(t.muted).child("本地归档账号下的文件夹；同名文件夹会直接追加进去。"));
        if let Phase::Running { percent, imported, skipped, .. } = &self.phase {
            body = body.child(
                div()
                    .mt(px(14.))
                    .child(div().h(px(6.)).rounded(px(3.)).bg(t.panel_2).child(div().h_full().rounded(px(3.)).bg(t.accent).w(relative(*percent as f32 / 100.))))
                    .child(div().mt(px(6.)).text_size(px(12.)).text_color(t.ink_2).child(format!(
                        "解析中 {percent}% · 已导入 {imported} 封{}",
                        if *skipped > 0 { format!(" · 跳过重复 {skipped} 封") } else { String::new() }
                    ))),
            );
        }
        if let Some(r) = &self.result {
            let folder = self.folder.read(cx).text();
            body = body.child(
                div()
                    .mt(px(14.))
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(t.panel_2)
                    .text_size(px(12.))
                    .line_height(px(20.))
                    .child(div().text_color(t.live).child(format!("导入完成：{} 封进入「{}」", r.imported, folder)))
                    .when(r.skipped > 0, |d| d.child(div().text_color(t.muted).child(format!("跳过 {} 封重复邮件（Message-ID 已存在）", r.skipped))))
                    .when(r.failed > 0, |d| {
                        d.child(div().text_color(t.danger).child(format!(
                            "{} 封解析失败{}",
                            r.failed,
                            r.errors.first().map(|e| format!("：{e}")).unwrap_or_default()
                        )))
                    }),
            );
        }
        if !self.error.is_empty() {
            body = body.child(div().mt(px(12.)).text_size(px(12.)).text_color(t.danger).child(self.error.clone()));
        }
        modal_shell(&t, 560., "导入邮件", ".eml / .mbox → 本地归档，不写回邮箱服务器", close).child(body).child(
            modal_footer(&t)
                .child(div().flex_1().text_size(px(11.5)).text_color(t.muted).child("导入的邮件只存在本机，不会被当成邮箱里的邮件去同步或标记。"))
                .child(disabled(button("imp-done", &t, Kind::Primary, None, if self.phase == Phase::Done { "完成" } else { "关闭" }, false), busy).on_click(
                    cx.listener(|m, _, _, cx| {
                        if !m.busy() {
                            cx.emit(ImportEvent::Close)
                        }
                    }),
                )),
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn importable_ext() {
        assert!(super::importable(std::path::Path::new("/a/b.MBOX")));
        assert!(super::importable(std::path::Path::new("x.eml")));
        assert!(!super::importable(std::path::Path::new("x.pdf")));
    }
}
