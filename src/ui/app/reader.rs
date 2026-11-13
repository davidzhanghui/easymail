//! 阅读区：工具栏、正文、会话、附件、安全提示。

use super::*;
use crate::core::{extract, parse as mparse};

/// 阅读区顶部的提示信息
#[derive(Default)]
pub struct Extras {
    pub code: Option<String>,
    pub trips: Vec<extract::Trip>,
    pub auth: mparse::AuthVerdict,
    pub phishing: Vec<(String, String)>,
    pub pixels: usize,
    pub tracker_hosts: Vec<String>,
}

impl Extras {
    pub fn of(d: &Detail) -> Extras {
        let (pixels, hosts) = match d.trackers.split_once('|') {
            Some((p, h)) => (p.parse().unwrap_or(0), h.split(',').filter(|x| !x.is_empty()).map(String::from).collect()),
            None => {
                let (h, p) = mparse::remote_trackers(&d.html);
                (p, h)
            }
        };
        Extras {
            code: extract::verification_code(&d.subject, &d.text),
            trips: extract::trips(&d.html, &d.text),
            auth: mparse::auth_verdict(&d.auth_res),
            phishing: extract::phishing_links(&d.html),
            pixels,
            tracker_hosts: hosts,
        }
    }
}

impl MailApp {
    fn extras_for(&mut self, d: &Detail) -> Rc<Extras> {
        match &self.extras {
            Some((id, len, e)) if *id == d.id && *len == d.html.len() + d.text.len() => e.clone(),
            _ => {
                let e = Rc::new(Extras::of(d));
                self.extras = Some((d.id, d.html.len() + d.text.len(), e.clone()));
                e
            }
        }
    }

    /// 发件人加入「始终显示远程图片」名单
    fn allow_sender_images(&mut self, address: String, cx: &mut Context<Self>) {
        let _ = self.store.w().execute(
            "INSERT OR IGNORE INTO remote_allow (address, created_at) VALUES (?, ?)",
            rusqlite::params![address.to_lowercase(), crate::core::db::now_ms()],
        );
        self.toast(format!("以后会自动显示 {address} 的图片"), false, cx);
        cx.notify();
    }

    pub(super) fn unsubscribe(&mut self, id: i64, cx: &mut Context<Self>) {
        let eng = self.eng.clone();
        self.toast("正在退订…", false, cx);
        self.run(async move { crate::core::tools::unsubscribe(&eng, id).await }, cx, |a, r, cx| match r {
            Ok(crate::core::tools::Unsub::Done) => a.toast("已退订（一键退订，对方服务器已确认）", false, cx),
            Ok(crate::core::tools::Unsub::Mailed(to)) => a.toast(format!("已向 {to} 发送退订邮件"), false, cx),
            Ok(crate::core::tools::Unsub::Open(url)) => {
                a.toast("这个发件人只支持网页退订，已在浏览器中打开", false, cx);
                cx.open_url(&url);
            }
            Err(e) => a.toast(format!("退订失败：{e:#}"), true, cx),
        });
    }

    fn render_extras(&self, t: &Theme, d: &Detail, x: &Extras, cx: &mut Context<Self>) -> Option<Div> {
        let mut rows: Vec<AnyElement> = Vec::new();
        let banner = |tone: Hsla, ic: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(12.))
                .py(px(7.))
                .rounded(px(8.))
                .bg(tone.opacity(0.10))
                .text_size(px(12.))
                .text_color(t.ink_2)
                .child(icon(ic, 13., tone))
        };
        if x.auth.failed() {
            rows.push(
                banner(t.danger, "shield")
                    .child(
                        crate::i18n::tr(&format!(
                            "发件人身份验证未通过（SPF {} · DKIM {} · DMARC {}），这封信可能是伪造的，请谨慎对待其中的链接和附件。",
                            or_dash(&x.auth.spf),
                            or_dash(&x.auth.dkim),
                            or_dash(&x.auth.dmarc)
                        ))
                        .into_owned(),
                    )
                    .into_any_element(),
            );
        }
        if !x.phishing.is_empty() {
            let (shown, real) = &x.phishing[0];
            rows.push(
                banner(t.danger, "shield")
                    .child(
                        crate::i18n::tr(&format!(
                            "可疑链接：显示为 {shown}，实际指向 {real}{}",
                            if x.phishing.len() > 1 { format!(" 等 {} 处", x.phishing.len()) } else { String::new() }
                        ))
                        .into_owned(),
                    )
                    .into_any_element(),
            );
        }
        if let Some((mid, inv, ics)) = self.invite.as_ref().filter(|(mid, ..)| *mid == d.id) {
            let mid = *mid;
            let mine = inv.my_status(&d.account_email);
            let saved = self.store.get_kv(&format!("rsvp.{}", inv.uid)).and_then(|s| calendar::Rsvp::from_partstat(&s));
            let status = saved.or_else(|| mine.and_then(|p| calendar::Rsvp::from_partstat(&p.partstat)));
            let can_reply = inv.method == "REQUEST" && inv.organizer.as_ref().is_some_and(|o| !o.email.is_empty()) && !d.account_local;
            let title = match inv.method.as_str() {
                "CANCEL" => "活动已取消",
                "REPLY" => "参会回复",
                _ => "日历邀请",
            };
            let mut who = div().flex().gap(px(6.)).flex_wrap().text_size(px(11.5)).text_color(t.muted);
            if let Some(o) = &inv.organizer {
                who = who.child(format!("组织者：{}", if o.name.is_empty() { o.email.clone() } else { o.name.clone() }));
            }
            if !inv.attendees.is_empty() {
                let acc = inv.attendees.iter().filter(|p| p.partstat == "ACCEPTED").count();
                who = who.child(format!("· {} 人受邀，{} 人已接受", inv.attendees.len(), acc));
            }
            let mut card = div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .px(px(12.))
                .py(px(10.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.border)
                .bg(t.panel)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(icon("clock", 14., if inv.method == "CANCEL" { t.danger } else { t.accent }))
                        .child(div().text_size(px(11.5)).text_color(t.muted).child(title))
                        .child(div().flex_1())
                        .when_some(status, |x, s| x.child(badge(t, format!("你{}", s.label()), false))),
                )
                .child(div().font_family(SERIF).text_size(px(16.)).font_weight(FontWeight::BOLD).text_color(t.ink).child(if inv.summary.is_empty() {
                    "(无标题活动)".to_string()
                } else {
                    inv.summary.clone()
                }))
                .child(
                    div().text_size(px(12.5)).text_color(t.ink_2).child(
                        crate::i18n::tr(&format!("时间：{}{}", inv.when_text(), if inv.rrule.is_empty() { String::new() } else { "（重复活动）".into() }))
                            .into_owned(),
                    ),
                );
            if !inv.location.is_empty() {
                card = card.child(div().text_size(px(12.5)).text_color(t.ink_2).child(format!("地点：{}", inv.location)));
            }
            card = card.child(who);
            let ics = ics.clone();
            let uid = inv.uid.clone();
            let mut btns = div().flex().gap(px(6.)).mt(px(6.));
            if can_reply {
                for (i, (r, label, kind)) in [
                    (calendar::Rsvp::Accept, "接受", Kind::Primary),
                    (calendar::Rsvp::Tentative, "待定", Kind::Secondary),
                    (calendar::Rsvp::Decline, "拒绝", Kind::Secondary),
                ]
                .into_iter()
                .enumerate()
                {
                    btns = btns.child(
                        button(SharedString::from(format!("rsvp-{i}")), t, kind, None, label, true)
                            .on_click(cx.listener(move |a, _, _, cx| a.rsvp(mid, r, cx))),
                    );
                }
            }
            if inv.method != "CANCEL" && inv.method != "REPLY" {
                btns = btns.child(button("cal-add", t, Kind::Ghost, Some("clock"), "添加到日历", true).on_click(cx.listener(move |a, _, _, cx| {
                    match calendar::add_to_calendar(&ics, &uid) {
                        Ok(()) => a.toast("已交给「日历」", false, cx),
                        Err(e) => a.toast(format!("{e:#}"), true, cx),
                    }
                })));
            }
            rows.push(card.child(btns).into_any_element());
        }
        if let Some(code) = &x.code {
            let c = code.clone();
            rows.push(
                banner(t.accent, "check")
                    .child("验证码")
                    .child(div().font_family(MONO).text_size(px(15.)).font_weight(FontWeight::BOLD).text_color(t.ink).child(code.clone()))
                    .child(div().flex_1())
                    .child(button("copy-code", t, Kind::Secondary, None, "复制", true).on_click(cx.listener(move |a, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(c.clone()));
                        a.toast("验证码已复制", false, cx);
                    })))
                    .into_any_element(),
            );
        }
        for (i, tr) in x.trips.iter().enumerate() {
            let kind = match tr.kind {
                "flight" => "航班",
                "train" => "火车",
                _ => "酒店",
            };
            let mut parts = vec![format!("{kind} {}", tr.title)];
            for p in [&tr.route, &tr.when] {
                if !p.trim().is_empty() && p.trim() != "→" {
                    parts.push(p.clone());
                }
            }
            if !tr.reference.is_empty() {
                parts.push(format!("订单 {}", tr.reference));
            }
            rows.push(banner(t.gold, "clock").id(("trip", i)).child(parts.join(" · ")).into_any_element());
        }
        if let Some(until) = d.snoozed_until {
            rows.push(
                banner(t.accent, "clock")
                    .child(format!("稍后处理：{} 重新出现在收件箱", fmt_full(until)))
                    .child(div().flex_1())
                    .child(button("unsnooze", t, Kind::Ghost, None, "取消", true).on_click(cx.listener({
                        let id = d.id;
                        move |a, _, _, cx| {
                            let _ = crate::core::snooze::unsnooze(&a.store, &[id]);
                            a.refresh_detail(cx);
                            a.load_list(true, false, cx);
                            a.load_meta(cx);
                        }
                    })))
                    .into_any_element(),
            );
        }
        if x.pixels > 0 {
            rows.push(
                banner(t.muted, "eyeOff")
                    .child(
                        crate::i18n::tr(&format!(
                            "已拦截 {} 个追踪像素（{}），对方无法得知你是否打开了这封信。",
                            x.pixels,
                            x.tracker_hosts.iter().take(3).cloned().collect::<Vec<_>>().join("、")
                        ))
                        .into_owned(),
                    )
                    .into_any_element(),
            );
        }
        if rows.is_empty() {
            return None;
        }
        Some(div().flex().flex_col().gap(px(6.)).mt(px(10.)).children(rows))
    }

    pub(super) fn render_reader(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let pane = div().flex().flex_col().flex_1().min_w_0().h_full().bg(t.bg);
        let d = match (&self.detail_state, self.detail.clone()) {
            (DetailState::Ready, Some(d)) => d,
            (state, _) => {
                webview::hide();
                let bar = div().flex().flex_none().justify_end().px(px(14.)).py(px(10.)).child(
                    button("toggle-reader", t, Kind::Ghost, Some("panel-right"), "", true).on_click(cx.listener(|a, _, _, cx| {
                        a.reader_collapsed = true;
                        a.save_layout();
                        cx.notify();
                    })),
                );
                return pane.child(bar).child(self.render_empty(t, state.clone(), cx)).into_any_element();
            }
        };
        let id = d.id;
        let local = d.account_local;
        let junk = d.folder_role == "junk";
        let has_archive = !local && d.folder_role != "archive" && self.has_role(d.account_id, "archive");
        let files: Vec<_> = d.attachments.iter().filter(|a| !a.inline).cloned().collect();
        let show_html = !d.html.is_empty() && !self.text_mode;
        let remote = self.remote_ok.contains(&id) || queries::remote_allowed(&self.store, &d.from.address);
        let extras = self.extras_for(&d);
        let extras_el = self.render_extras(t, &d, &extras, cx);
        let unsub = !d.unsub.is_empty() && !local;
        let from_addr = d.from.address.clone();
        let has_remote = match self.remote_cache {
            Some((cid, len, v)) if cid == id && len == d.html.len() => v,
            _ => {
                let v = webview::has_remote_content(&d.html);
                self.remote_cache = Some((id, d.html.len(), v));
                v
            }
        };
        let blocked = show_html && !remote && has_remote;
        let purge = d.folder_role == "trash" || local;

        // 深色主题下反色显示 HTML 邮件（设置可关；工具栏可对当前这封切回原样）
        let dark_mail = t.dark && show_html && self.store.get_kv("dark_mail").as_deref() != Some("0") && !self.mail_light.contains(&id);
        // 文档缓存：换信 / 换模式 / 放行图片 / 换主题才重建
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (id, show_html, remote, t.id, d.html.len(), d.text.len(), dark_mail).hash(&mut h);
            h.finish()
        };
        if self.doc.as_ref().map(|x| x.0) != Some(key) {
            let doc = if show_html {
                webview::mail_document(&d.html, remote, dark_mail)
            } else {
                webview::text_document(&d.text, &hex_css(t.hex_ink), &hex_css(t.hex_paper), &hex_css(t.hex_muted), &hex_css(t.hex_accent))
            };
            self.doc = Some((key, doc.into()));
        }
        let doc = self.doc.as_ref().map(|x| x.1.clone()).unwrap_or_else(|| Arc::from(""));
        let frozen_snap = self.freeze.snapshot().filter(|_| self.overlay_open());

        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .px(px(22.))
            .h(px(52.))
            .border_b_1()
            .border_color(t.border_soft)
            .child(button("t-reply", t, Kind::Secondary, Some("reply"), "回复", true).on_click(cx.listener(|a, _, w, cx| a.reply("reply", w, cx))))
            .child(button("t-replyall", t, Kind::Secondary, Some("replyAll"), "全部回复", true).on_click(cx.listener(|a, _, w, cx| a.reply("replyAll", w, cx))))
            .child(button("t-forward", t, Kind::Secondary, Some("forward"), "转发", true).on_click(cx.listener(|a, _, w, cx| a.reply("forward", w, cx))))
            .child(div().w(px(6.)))
            .child(
                button("t-star", t, Kind::Secondary, Some(if d.flagged { "star-fill" } else { "star" }), "", true)
                    .text_color(if d.flagged { t.gold } else { t.ink_2 })
                    .on_click(cx.listener(move |a, _, _, cx| {
                        let f = a.detail.as_ref().is_some_and(|d| d.flagged);
                        a.set_flagged(vec![id], !f, false, cx)
                    })),
            )
            .child(button("t-seen", t, Kind::Secondary, Some(if d.seen { "unread" } else { "read" }), "", true).on_click(cx.listener(move |a, _, _, cx| {
                let s = a.detail.as_ref().is_some_and(|d| d.seen);
                a.set_seen(vec![id], !s, cx)
            })))
            .child(
                button("t-move", t, Kind::Secondary, Some("move"), "", true)
                    .on_click(cx.listener(move |a, ev: &ClickEvent, _, cx| a.toggle_pop(Pop::Move(vec![id]), ev.position(), cx))),
            )
            .when(has_archive, |x| {
                x.child(
                    button("t-archive", t, Kind::Secondary, Some("archive"), "", true)
                        .on_click(cx.listener(move |a, _, _, cx| a.to_role(vec![id], "archive", "已归档", cx))),
                )
            })
            .when(!local, |x| {
                x.child(button("t-spam", t, Kind::Secondary, Some(if junk { "inbox" } else { "spam" }), "", true).on_click(cx.listener(move |a, _, _, cx| {
                    if junk {
                        a.to_role(vec![id], "inbox", "已移回收件箱", cx)
                    } else {
                        a.to_role(vec![id], "junk", "已标为垃圾邮件", cx)
                    }
                })))
            })
            .child(
                button("t-snooze", t, Kind::Secondary, Some("clock"), "", true)
                    .on_click(cx.listener(move |a, ev: &ClickEvent, _, cx| a.toggle_pop(Pop::Snooze(vec![id]), ev.position(), cx))),
            )
            .child({
                let subj = d.subject.clone();
                button("t-eml", t, Kind::Secondary, Some("download"), "", true).on_click(cx.listener(move |a, _, _, cx| a.save_eml(id, &subj, cx)))
            })
            .when(unsub, |x| x.child(button("t-unsub", t, Kind::Ghost, None, "退订", true).on_click(cx.listener(move |a, _, _, cx| a.unsubscribe(id, cx)))))
            .child(div().flex_1())
            .child(button("toggle-reader", t, Kind::Ghost, Some("panel-right"), "", true).on_click(cx.listener(|a, _, _, cx| {
                a.reader_collapsed = true;
                a.save_layout();
                cx.notify();
            })))
            .child(
                button("t-delete", t, Kind::Danger, Some("trash"), if purge { "彻底删除" } else { "删除" }, true)
                    .on_click(cx.listener(move |a, _, w, cx| a.delete(vec![id], w, cx))),
            );

        let meta_row = |k: &'static str, v: AnyElement| {
            div()
                .flex()
                .gap(px(8.))
                .child(div().w(px(52.)).flex_none().text_color(t.muted).child(crate::i18n::tr(k).into_owned()))
                .child(div().flex_1().min_w_0().child(v))
        };
        let mono = |s: String| div().font_family(MONO).text_size(px(11.5)).child(s).into_any_element();
        let header = div()
            .flex_none()
            .px(px(36.))
            .pt(px(24.))
            .child(
                div()
                    .font_family(SERIF)
                    .text_size(px(24.))
                    .font_weight(FontWeight::BOLD)
                    .line_height(px(32.))
                    .text_color(t.ink)
                    .child(if d.subject.is_empty() { "(无主题)".to_string() } else { d.subject.clone() }),
            )
            .child(
                div()
                    .mt(px(12.))
                    .text_size(px(12.5))
                    .line_height(px(23.))
                    .text_color(t.ink_2)
                    .child(meta_row(
                        "发件人",
                        div().flex().gap(px(6.)).child(d.from.name.clone()).child(mono(format!("<{}>", d.from.address))).into_any_element(),
                    ))
                    .child(meta_row("收件人", mono(if d.to.is_empty() { "—".into() } else { addrs_text(&d.to) })))
                    .when(!d.cc.is_empty(), |x| x.child(meta_row("抄送", mono(addrs_text(&d.cc)))))
                    .when(!d.reply_to.is_empty() && d.reply_to != d.from.address, |x| x.child(meta_row("回复至", mono(d.reply_to.clone()))))
                    .child(meta_row("时间", div().child(fmt_full(d.date)).into_any_element()))
                    .child(meta_row(
                        "位置",
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(dot(t.named(&d.account_color), 7.))
                            .child(
                                crate::i18n::tr(&format!("{} / {}", if local { "本地归档".to_string() } else { d.account_email.clone() }, d.folder_label))
                                    .into_owned(),
                            )
                            .into_any_element(),
                    )),
            )
            .child(div().h(px(1.)).bg(t.border_soft).mt(px(14.)))
            .children(extras_el)
            .when(!d.html.is_empty(), |x| {
                x.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .mt(px(10.))
                        .px(px(12.))
                        .h(px(36.))
                        .rounded(px(8.))
                        .when(blocked, |x| x.bg(t.panel_2))
                        .text_size(px(12.))
                        .text_color(t.ink_2)
                        .when(blocked, |x| x.child(icon("image", 13., t.muted)).child("为保护隐私，已阻止加载远程图片与追踪像素。"))
                        .child(div().flex_1())
                        .when(blocked, |x| {
                            x.child(button("show-images", t, Kind::Secondary, None, "显示图片", true).on_click(cx.listener(move |a, _, _, cx| {
                                a.remote_ok.insert(id);
                                cx.notify();
                            })))
                            .when(!from_addr.is_empty(), |x| {
                                let addr = from_addr.clone();
                                x.child(
                                    button("allow-sender", t, Kind::Ghost, None, "总是显示此发件人", true)
                                        .on_click(cx.listener(move |a, _, _, cx| a.allow_sender_images(addr.clone(), cx))),
                                )
                            })
                        })
                        .when(t.dark && show_html, |x| {
                            let light = self.mail_light.contains(&id);
                            x.child(button("toggle-dark", t, Kind::Ghost, None, if light { "反色显示" } else { "原样显示" }, true).on_click(cx.listener(
                                move |a, _, _, cx| {
                                    if !a.mail_light.remove(&id) {
                                        a.mail_light.insert(id);
                                    }
                                    cx.notify();
                                },
                            )))
                        })
                        .child(button("toggle-mode", t, Kind::Ghost, None, if self.text_mode { "原始排版" } else { "纯文本" }, true).on_click(cx.listener(
                            |a, _, _, cx| {
                                a.text_mode = !a.text_mode;
                                cx.notify();
                            },
                        ))),
                )
            });

        let body = div()
            .id("mail-body")
            .flex_1()
            .min_h(px(120.))
            .mx(px(if show_html { 24. } else { 32. }))
            .mt(px(10.))
            .relative()
            .rounded(px(6.))
            .overflow_hidden()
            .child(
                canvas(
                    |b, _, _| b,
                    move |b, _, window, _| {
                        webview::show(window, b, key, &doc);
                    },
                )
                .size_full(),
            )
            .when_some(frozen_snap, |x, s| x.child(img(s).absolute().top_0().left_0().size_full()));

        let mut footer = div().flex_none().px(px(36.)).pb(px(16.)).pt(px(10.)).flex().flex_col().gap(px(10.));
        if !files.is_empty() {
            let mut row = div().flex().flex_wrap().gap(px(8.));
            for a in files.iter() {
                let (ix, name, size) = (a.index, a.filename.clone(), a.size);
                row = row.child(
                    button(SharedString::from(format!("att-{ix}")), t, Kind::Secondary, Some("clip"), one_line(&a.filename, 36), true)
                        .child(div().text_size(px(10.5)).text_color(t.muted).child(fmt_size(a.size)))
                        .on_click(cx.listener(move |app, _, _, cx| app.open_preview(id, ix, name.clone(), size, cx))),
                );
            }
            footer = footer.child(div().text_size(px(11.)).text_color(t.muted).child(format!("附  件 · {}", files.len()))).child(row);
        }
        if d.thread.len() > 1 {
            let n = d.thread.len();
            let pos = d.thread.iter().position(|t| t.id == id).unwrap_or(0);
            let archive_ids = if has_archive { self.thread_ids(&d, true) } else { vec![] };
            let delete_ids = self.thread_ids(&d, false);
            let all_ids: Vec<i64> = d.thread.iter().map(|t| t.id).collect();
            let any_unread = d.thread.iter().any(|t| !t.seen && t.id != id);
            let tbtn = |bid: &'static str, label: &'static str, off: bool| disabled(button(bid, t, Kind::Ghost, None, label, true), off);
            let head = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(2.))
                .child(div().flex_1().min_w(px(80.)).text_size(px(11.)).text_color(t.muted).child(format!("同一会话 · 第 {} / {} 封", pos + 1, n)))
                .child(tbtn("th-prev", "上一封 [", pos == 0).on_click(cx.listener(|a, _, _, cx| a.go_in_thread(-1, cx))))
                .child(tbtn("th-next", "下一封 ]", pos + 1 >= n).on_click(cx.listener(|a, _, _, cx| a.go_in_thread(1, cx))))
                .child(tbtn("th-read", "全部已读", !any_unread).on_click(cx.listener(move |a, _, _, cx| a.set_seen(all_ids.clone(), true, cx))))
                .child(
                    tbtn("th-arch", "归档会话", archive_ids.is_empty())
                        .on_click(cx.listener(move |a, _, _, cx| a.to_role(archive_ids.clone(), "archive", "已归档会话", cx))),
                )
                .child(tbtn("th-del", "删除会话", delete_ids.is_empty()).on_click(cx.listener(move |a, _, w, cx| a.delete(delete_ids.clone(), w, cx))));
            if self.thread_scrolled != Some(id) {
                self.thread_scrolled = Some(id);
                self.thread_scroll.scroll_to_item(pos);
            }
            let mut list = div().id("thread").flex().flex_col().gap(px(2.)).max_h(px(168.)).overflow_y_scroll().track_scroll(&self.thread_scroll);
            for th in d.thread.iter() {
                let tid = th.id;
                let cur = tid == id;
                // 当前这封正在阅读，必然是已读（库里的 seen 可能还没刷新）
                let unread = !th.seen && !cur;
                list = list.child(
                    div()
                        .id(("th", tid as u64))
                        .flex()
                        .items_start()
                        .gap(px(8.))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(7.))
                        .cursor_pointer()
                        .border_l_2()
                        .border_color(if cur { t.accent } else { gpui::transparent_black() })
                        .when(cur, |x| x.bg(t.accent_soft))
                        .hover(|s| s.bg(if cur { t.accent_soft } else { t.row_hover }))
                        .child(div().mt(px(6.)).size(px(7.)).flex_none().rounded_full().when(unread, |x| x.bg(t.accent)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        .text_size(px(12.5))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .whitespace_nowrap()
                                                .text_ellipsis()
                                                .text_color(t.ink)
                                                .when(unread, |x| x.font_weight(FontWeight::SEMIBOLD))
                                                .child(th.from.short().to_string()),
                                        )
                                        .child(div().flex_none().text_size(px(11.)).text_color(t.muted).child(format!(
                                            "{} · {}",
                                            th.folder,
                                            fmt_full(th.date)
                                        ))),
                                )
                                .child(
                                    div()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_size(px(12.))
                                        .text_color(t.muted)
                                        .child(if th.snippet.trim().is_empty() { th.subject.clone() } else { one_line(&th.snippet, 120) }),
                                ),
                        )
                        .on_click(cx.listener(move |a, _, _, cx| {
                            if !cur {
                                a.open_message(tid, cx)
                            }
                        })),
                );
            }
            footer = footer.child(head).child(list);
        }

        let card = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .max_w(px(880.))
            .mx_auto()
            .my(px(18.))
            .bg(t.paper)
            .border_1()
            .border_color(t.border)
            .rounded(px(12.))
            .overflow_hidden()
            .child(header)
            .child(body)
            .child(footer);

        pane.child(toolbar).child(div().flex().flex_1().min_h_0().px(px(22.)).child(card)).into_any_element()
    }

    pub(super) fn render_empty(&mut self, t: &Theme, state: DetailState, cx: &mut Context<Self>) -> impl IntoElement {
        let has_accounts = self.accounts.iter().any(|a| !a.is_local());
        let base = div().flex().flex_col().flex_1().items_center().justify_center().px(px(40.)).text_color(t.ink);
        let envelope = || div().mb(px(22.)).child(icon("envelope", 54., t.line_2).with_transformation(Transformation::rotate(radians(-0.12))));
        match state {
            DetailState::Loading => base.child(envelope()).child(div().text_size(px(13.)).text_color(t.ink_2).child("正在取回这封信…")),
            DetailState::Error(e) => base.child(envelope()).child(div().text_size(px(13.)).text_color(t.danger).max_w(px(460.)).child(e)),
            _ if has_accounts || self.stats.archived > 0 => base
                .child(envelope())
                .child(div().font_family(SERIF).text_size(px(11.)).text_color(t.muted).mb(px(18.)).child("A  L E T T E R  F R O M  T H E  P A S T"))
                .child(div().font_family(SERIF).text_size(px(32.)).font_weight(FontWeight::BOLD).mb(px(22.)).child("把时间，慢慢读回来。"))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(t.ink_2)
                        .line_height(px(25.))
                        .text_center()
                        .child("选择一封邮件，开始阅读。")
                        .child("所有邮件都会增量存档到本机，断网也能读、能搜。"),
                )
                .child(div().w_full().max_w(px(540.)).h(px(1.)).bg(t.border).mt(px(38.)).mb(px(28.)))
                .child(
                    div()
                        .flex()
                        .gap(px(64.))
                        .child(stat(t, fmt_num(self.stats.archived), "封已存档"))
                        .child(stat(t, self.accounts.iter().filter(|a| !a.is_local()).count().to_string(), "个账号"))
                        .child(stat(t, fmt_num(self.stats.unread), "封未读")),
                ),
            _ => base
                .child(div().mb(px(22.)).child(logo(72.)))
                .child(div().font_family(SERIF).text_size(px(11.)).text_color(t.muted).mb(px(18.)).child("E A S Y M A I L  ·  迷 你 邮 件"))
                .child(div().font_family(SERIF).text_size(px(32.)).font_weight(FontWeight::BOLD).mb(px(22.)).child("先接入一个邮箱。"))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(t.ink_2)
                        .line_height(px(25.))
                        .text_center()
                        .child("支持 Gmail（应用专用密码 + 代理）、网易 163、QQ 邮箱（授权码）与 AgentMail。")
                        .child("凭据 AES-256-GCM 加密保存在本机。"),
                )
                .child(
                    button("empty-add", t, Kind::Primary, Some("plus"), "添加邮箱账号", false)
                        .mt(px(26.))
                        .on_click(cx.listener(|a, _, w, cx| a.open_accounts(Tab::Accounts, true, w, cx))),
                ),
        }
    }
}

fn or_dash(s: &str) -> &str {
    if s.is_empty() {
        "—"
    } else {
        s
    }
}

// ───────────────────────── 会话时间线 ─────────────────────────

/// 会话级「归档 / 删除」不动的文件夹：已发送、草稿、垃圾箱、垃圾邮件（删除时）、已归档（归档时）
fn thread_actionable(role: &str, archive: bool) -> bool {
    !matches!(role, "sent" | "drafts" | "trash" | "junk") && !(archive && role == "archive")
}

impl MailApp {
    /// 在当前会话里跳到上一封 / 下一封（快捷键 [ ]）
    pub fn go_in_thread(&mut self, step: i32, cx: &mut Context<Self>) {
        let Some(d) = self.detail.clone().filter(|_| self.detail_state == DetailState::Ready) else { return };
        let Some(pos) = d.thread.iter().position(|t| t.id == d.id) else { return };
        let to = pos as i32 + step;
        if to < 0 || to as usize >= d.thread.len() {
            return;
        }
        self.open_message(d.thread[to as usize].id, cx);
    }

    /// 会话里可以被归档的邮件 id（排除已发送 / 草稿 / 垃圾箱 / 已在归档里的；本地归档账号不动）
    fn thread_ids(&self, d: &Detail, archive: bool) -> Vec<i64> {
        if d.account_local {
            return vec![];
        }
        d.thread.iter().filter(|t| thread_actionable(&t.role, archive)).map(|t| t.id).collect()
    }
}
