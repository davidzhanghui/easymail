//! 联系人：往来统计从邮件里实时算（收到的、发出的、最近一次），用户可以补充姓名 / 公司 / 电话 / 备注 / 星标，
//! 也能新建没有邮件往来的联系人、导入 / 导出 vCard。手动信息存在 `contacts` 表，不改邮件数据。

use super::db::{now_ms, Store};
use super::queries::like;
use anyhow::{bail, Result};
use rusqlite::params;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContactRow {
    pub address: String,
    /// 手动填的姓名优先，其次是邮件里最常见的显示名
    pub name: String,
    pub org: String,
    pub phone: String,
    pub note: String,
    pub starred: bool,
    /// 收到几封
    pub received: i64,
    /// 发出几封（收件人 / 抄送里有他）
    pub sent: i64,
    pub last: i64,
}

impl ContactRow {
    pub fn display(&self) -> &str {
        if self.name.is_empty() {
            &self.address
        } else {
            &self.name
        }
    }
}

/// 联系人列表。`q` 匹配姓名 / 地址 / 公司；星标在前，其次按往来封数。不含自己的账号地址
pub fn list(store: &Store, q: &str, limit: usize) -> Result<Vec<ContactRow>> {
    let k = like(q.trim());
    let c = store.r();
    let mut st = c.prepare(
        "WITH seen AS (
           SELECT lower(m.from_addr) address, MAX(m.from_name) name, COUNT(*) received, 0 sent, MAX(m.date) last
             FROM messages m JOIN folders f ON f.id = m.folder_id
            WHERE m.from_addr != '' AND f.role NOT IN ('sent','drafts','trash','junk') GROUP BY lower(m.from_addr)
           UNION ALL
           SELECT lower(json_extract(j.value, '$.address')), MAX(json_extract(j.value, '$.name')), 0, COUNT(*), MAX(m.date)
             FROM messages m JOIN folders f ON f.id = m.folder_id, json_each(m.to_json) j
            WHERE f.role = 'sent' AND json_extract(j.value, '$.address') IS NOT NULL GROUP BY lower(json_extract(j.value, '$.address'))
         ), agg AS (
           SELECT address, MAX(name) name, SUM(received) received, SUM(sent) sent, MAX(last) last FROM seen
            WHERE address != '' AND address NOT IN (SELECT lower(email) FROM accounts) GROUP BY address
         ), all_addr AS (
           SELECT address FROM agg UNION SELECT lower(address) FROM contacts
         )
         SELECT a.address, COALESCE(NULLIF(c.name, ''), g.name, ''), COALESCE(c.org, ''), COALESCE(c.phone, ''), COALESCE(c.note, ''), COALESCE(c.starred, 0),
                COALESCE(g.received, 0), COALESCE(g.sent, 0), COALESCE(g.last, 0)
           FROM all_addr a LEFT JOIN agg g ON g.address = a.address LEFT JOIN contacts c ON lower(c.address) = a.address
          WHERE (?1 = '%%' OR a.address LIKE ?1 ESCAPE '\\' OR COALESCE(NULLIF(c.name, ''), g.name, '') LIKE ?1 ESCAPE '\\' OR COALESCE(c.org, '') LIKE ?1 ESCAPE '\\')
          ORDER BY COALESCE(c.starred, 0) DESC, COALESCE(g.received, 0) + COALESCE(g.sent, 0) * 3 DESC, a.address LIMIT ?2",
    )?;
    let rows = st.query_map(params![k, limit as i64], |r| {
        Ok(ContactRow {
            address: r.get(0)?,
            name: r.get(1)?,
            org: r.get(2)?,
            phone: r.get(3)?,
            note: r.get(4)?,
            starred: r.get::<_, i64>(5)? != 0,
            received: r.get(6)?,
            sent: r.get(7)?,
            last: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn valid_addr(a: &str) -> bool {
    let a = a.trim();
    a.len() >= 3 && !a.contains(char::is_whitespace) && a.split_once('@').is_some_and(|(u, d)| !u.is_empty() && d.contains('.') && !d.ends_with('.'))
}

/// 新建或更新（按地址，不区分大小写）。星标沿用已有值
pub fn save(store: &Store, c: &ContactRow) -> Result<()> {
    let addr = c.address.trim().to_lowercase();
    if !valid_addr(&addr) {
        bail!("邮箱地址不正确：{}", c.address.trim());
    }
    store.w().execute(
        "INSERT INTO contacts(address, name, org, phone, note, starred, created_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(address) DO UPDATE SET name = ?2, org = ?3, phone = ?4, note = ?5, starred = ?6",
        params![addr, c.name.trim(), c.org.trim(), c.phone.trim(), c.note.trim(), c.starred as i64, now_ms()],
    )?;
    Ok(())
}

pub fn set_starred(store: &Store, address: &str, starred: bool) -> Result<()> {
    let addr = address.trim().to_lowercase();
    store.w().execute(
        "INSERT INTO contacts(address, starred, created_at) VALUES(?1, ?2, ?3) ON CONFLICT(address) DO UPDATE SET starred = ?2",
        params![addr, starred as i64, now_ms()],
    )?;
    Ok(())
}

/// 删掉手动信息（邮件里出现过的地址仍会出现在列表里，只是回到自动识别的姓名）
pub fn delete(store: &Store, address: &str) -> Result<()> {
    store.w().execute("DELETE FROM contacts WHERE lower(address) = ?", [address.trim().to_lowercase()])?;
    Ok(())
}

// ───────────────────────────── vCard ─────────────────────────────

fn vesc(s: &str) -> String {
    s.replace('\\', "\\\\").replace(',', "\\,").replace(';', "\u{5c};").replace('\n', "\\n")
}

fn vunesc(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// 导出 vCard 3.0（Apple 通讯录 / Google 联系人都能导入）
pub fn to_vcard(rows: &[ContactRow]) -> String {
    let mut out = String::new();
    for r in rows {
        out.push_str("BEGIN:VCARD\r\nVERSION:3.0\r\n");
        out.push_str(&format!("FN:{}\r\n", vesc(r.display())));
        out.push_str(&format!("N:{};;;;\r\n", vesc(r.display())));
        out.push_str(&format!("EMAIL;TYPE=INTERNET:{}\r\n", r.address));
        if !r.org.is_empty() {
            out.push_str(&format!("ORG:{}\r\n", vesc(&r.org)));
        }
        if !r.phone.is_empty() {
            out.push_str(&format!("TEL:{}\r\n", vesc(&r.phone)));
        }
        if !r.note.is_empty() {
            out.push_str(&format!("NOTE:{}\r\n", vesc(&r.note)));
        }
        out.push_str("END:VCARD\r\n");
    }
    out
}

/// 解析 vCard（2.1 / 3.0 / 4.0 的常用字段）。一张卡有多个邮箱就拆成多个联系人；没有邮箱的卡跳过
pub fn from_vcard(text: &str) -> Vec<ContactRow> {
    let mut unfolded: Vec<String> = Vec::new();
    for l in text.replace("\r\n", "\n").split('\n') {
        if (l.starts_with(' ') || l.starts_with('\t')) && !unfolded.is_empty() {
            unfolded.last_mut().expect("last").push_str(&l[1..]);
        } else {
            unfolded.push(l.to_string());
        }
    }
    let mut out = Vec::new();
    let (mut name, mut org, mut phone, mut note) = (String::new(), String::new(), String::new(), String::new());
    let mut emails: Vec<String> = Vec::new();
    let mut in_card = false;
    for l in unfolded {
        let Some((head, value)) = l.split_once(':') else { continue };
        let key = head.split(';').next().unwrap_or("").to_uppercase();
        let key = key.rsplit('.').next().unwrap_or("").to_string(); // item1.EMAIL → EMAIL
        match key.as_str() {
            "BEGIN" if value.eq_ignore_ascii_case("VCARD") => {
                in_card = true;
                (name, org, phone, note) = Default::default();
                emails.clear();
            }
            "END" if value.eq_ignore_ascii_case("VCARD") && in_card => {
                in_card = false;
                for e in emails.drain(..) {
                    out.push(ContactRow {
                        address: e.to_lowercase(),
                        name: name.clone(),
                        org: org.clone(),
                        phone: phone.clone(),
                        note: note.clone(),
                        ..Default::default()
                    });
                }
            }
            _ if !in_card => {}
            "FN" => name = vunesc(value.trim()),
            "N" if name.is_empty() => {
                // 姓;名;…：中文通常「姓+名」连写，其他语言「名 姓」
                let p: Vec<&str> = value.split(';').collect();
                let (fam, giv) = (vunesc(p.first().copied().unwrap_or("")), vunesc(p.get(1).copied().unwrap_or("")));
                name = format!("{fam}{giv}").trim().to_string();
            }
            "ORG" => org = vunesc(value.split(';').next().unwrap_or("").trim()),
            "TEL" if phone.is_empty() => phone = vunesc(value.trim()),
            "NOTE" => note = vunesc(value.trim()),
            "EMAIL" if valid_addr(value) => emails.push(value.trim().to_string()),
            _ => {}
        }
    }
    out
}

/// 导入：已存在的联系人只补充空白字段，不覆盖用户已填的内容。返回新增 / 补充了几个
pub fn import(store: &Store, rows: &[ContactRow]) -> Result<usize> {
    let mut n = 0;
    for r in rows {
        let addr = r.address.to_lowercase();
        let cur = list(store, &addr, 5)?.into_iter().find(|c| c.address == addr);
        let merged = match cur {
            Some(c) => ContactRow {
                address: addr.clone(),
                name: if c.name.is_empty() { r.name.clone() } else { c.name.clone() },
                org: if c.org.is_empty() { r.org.clone() } else { c.org.clone() },
                phone: if c.phone.is_empty() { r.phone.clone() } else { c.phone.clone() },
                note: if c.note.is_empty() { r.note.clone() } else { c.note.clone() },
                starred: c.starred,
                ..Default::default()
            },
            None => ContactRow { address: addr.clone(), ..r.clone() },
        };
        save(store, &merged)?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("data")).unwrap();
        crate::core::queries::tests::seed(&s);
        (dir, s)
    }

    #[test]
    fn list_counts_and_overrides() {
        let (_d, s) = seeded();
        let all = list(&s, "", 50).unwrap();
        let bob = all.iter().find(|c| c.address == "bob@ex.com").unwrap();
        // seed：Bob 发来 1 封，我回复他 1 封
        assert_eq!((bob.received, bob.sent), (1, 1));
        assert_eq!(bob.name, "Bob");
        // 自己的账号地址、垃圾箱里的发件人不出现
        assert!(!all.iter().any(|c| c.address == "me@x.com" || c.address == "s@s.com"));
        // 手动改名 + 星标：星标排最前，搜索能按新名字找到
        save(&s, &ContactRow { address: "LI@ex.com".into(), name: "李老师".into(), org: "示例学校".into(), ..Default::default() }).unwrap();
        set_starred(&s, "li@ex.com", true).unwrap();
        let all = list(&s, "", 50).unwrap();
        assert_eq!(all[0].address, "li@ex.com");
        assert_eq!((all[0].name.as_str(), all[0].org.as_str(), all[0].starred), ("李老师", "示例学校", true));
        assert_eq!(list(&s, "示例学校", 50).unwrap().len(), 1);
        assert_eq!(list(&s, "bob@", 50).unwrap().len(), 1);
        // 没有邮件往来的手动联系人也在列表里
        save(&s, &ContactRow { address: "new@friend.org".into(), name: "新朋友".into(), ..Default::default() }).unwrap();
        assert!(list(&s, "新朋友", 5).unwrap().iter().any(|c| c.address == "new@friend.org" && c.received == 0));
        // 删除手动信息：有往来的回到自动识别
        delete(&s, "li@ex.com").unwrap();
        assert_eq!(list(&s, "li@ex.com", 5).unwrap()[0].name, "李四");
        assert!(save(&s, &ContactRow { address: "bad".into(), ..Default::default() }).is_err());
    }

    #[test]
    fn vcard_roundtrip_and_import_merge() {
        let rows = vec![ContactRow {
            address: "a@b.cn".into(),
            name: "张三, 先生".into(),
            org: "公司;部门".into(),
            phone: "138".into(),
            note: "第一行\n第二行".into(),
            ..Default::default()
        }];
        let v = to_vcard(&rows);
        assert!(v.contains("BEGIN:VCARD") && v.contains("EMAIL;TYPE=INTERNET:a@b.cn"));
        let back = from_vcard(&v);
        assert_eq!(back.len(), 1);
        assert_eq!((back[0].name.as_str(), back[0].note.as_str(), back[0].phone.as_str()), ("张三, 先生", "第一行\n第二行", "138"));
        // 2.1 风格 + 折行 + N 字段 + 两个邮箱 + 没邮箱的卡
        let vcf = "BEGIN:VCARD\nVERSION:2.1\nN:王;五;;;\nEMAIL;HOME:w1@x.com\nitem1.EMAIL;type=WORK:w2@x.com\nORG:某某\n 公司;研发\nEND:VCARD\nBEGIN:VCARD\nFN:没邮箱\nEND:VCARD\n";
        let cards = from_vcard(vcf);
        assert_eq!(cards.len(), 2);
        assert_eq!((cards[0].name.as_str(), cards[0].org.as_str()), ("王五", "某某公司"));
        // 导入合并：已有字段不被覆盖
        let (_d, s) = seeded();
        save(&s, &ContactRow { address: "w1@x.com".into(), name: "老王".into(), ..Default::default() }).unwrap();
        import(&s, &cards).unwrap();
        let w1 = list(&s, "w1@x.com", 5).unwrap().into_iter().next().unwrap();
        assert_eq!((w1.name.as_str(), w1.org.as_str()), ("老王", "某某公司"));
    }
}
