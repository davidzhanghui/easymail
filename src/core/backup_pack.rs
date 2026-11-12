//! 加密备份与一键恢复。
//!
//! - **加密备份**：把 [`archive::backup`] 生成的备份目录打成 tar，再用密码加密成单个 `.embak` 文件。
//!   密钥由 PBKDF2-HMAC-SHA256（60 万轮）从密码派生，数据按 1 MiB 分块用 AES-256-GCM 加密：
//!   每块的 nonce 是块序号，AAD 是「是否最后一块」，所以块不能被重排、删除或截断。
//!   备份里的 `master.key` 也在密文里，备份放在云盘上不会把密钥和密文一起暴露。
//! - **恢复**：校验后先解到数据目录旁的 `restore-pending/`，应用退出重启时由 [`apply_pending`]
//!   把旧数据目录改名成 `data.before-restore-<时间>`（不删除），再把暂存目录换上去。
//!
//! 文件格式：`"EMBK\x01"` | 盐 16 字节 | 轮数 u32 | 若干块 `[末块标记 u8][密文长度 u32][密文+tag]`（大端）。

use super::archive::{self, BackupReport};
use super::db::{now_ms, Store, DB_NAME, MIGRATIONS};
use super::transfer;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use rand::RngCore;
use ring::pbkdf2;
use std::io::{self, Read, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

pub const EXT: &str = "embak";
const MAGIC: &[u8; 5] = b"EMBK\x01";
const CHUNK: usize = 1 << 20;
const MAX_CHUNK: usize = 64 << 20;
const ITER: u32 = 600_000;
const WRONG: &str = "密码错误，或备份文件已损坏";

fn derive(password: &str, salt: &[u8], iter: u32) -> Aes256Gcm {
    let mut key = [0u8; 32];
    pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA256, NonZeroU32::new(iter).expect("iter"), salt, password.as_bytes(), &mut key);
    Aes256Gcm::new_from_slice(&key).expect("key")
}

fn nonce(counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_be_bytes());
    n
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

struct EncWriter<W: Write> {
    out: W,
    cipher: Aes256Gcm,
    buf: Vec<u8>,
    counter: u64,
}

impl<W: Write> EncWriter<W> {
    fn new(mut out: W, password: &str) -> io::Result<Self> {
        let mut salt = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        out.write_all(MAGIC)?;
        out.write_all(&salt)?;
        out.write_all(&ITER.to_be_bytes())?;
        Ok(EncWriter { out, cipher: derive(password, &salt, ITER), buf: Vec::with_capacity(CHUNK + 4096), counter: 0 })
    }

    fn seal(&mut self, last: bool) -> io::Result<()> {
        let ct = self.cipher.encrypt(Nonce::from_slice(&nonce(self.counter)), Payload { msg: &self.buf, aad: &[last as u8] }).map_err(|_| bad("加密失败"))?;
        self.out.write_all(&[last as u8])?;
        self.out.write_all(&(ct.len() as u32).to_be_bytes())?;
        self.out.write_all(&ct)?;
        self.counter += 1;
        self.buf.clear();
        Ok(())
    }

    fn finish(mut self) -> io::Result<W> {
        self.seal(true)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

impl<W: Write> Write for EncWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.seal(false)?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct DecReader<R: Read> {
    inp: R,
    cipher: Aes256Gcm,
    buf: Vec<u8>,
    pos: usize,
    counter: u64,
    done: bool,
}

impl<R: Read> DecReader<R> {
    fn new(mut inp: R, password: &str) -> io::Result<Self> {
        let mut head = [0u8; 5 + 16 + 4];
        inp.read_exact(&mut head).map_err(|_| bad("不是 EasyMail 加密备份"))?;
        if &head[..5] != MAGIC {
            return Err(bad("不是 EasyMail 加密备份"));
        }
        let iter = u32::from_be_bytes(head[21..25].try_into().expect("4"));
        if !(1..=10_000_000).contains(&iter) {
            return Err(bad("备份文件头损坏"));
        }
        Ok(DecReader { inp, cipher: derive(password, &head[5..21], iter), buf: Vec::new(), pos: 0, counter: 0, done: false })
    }

    fn next_chunk(&mut self) -> io::Result<()> {
        let mut h = [0u8; 5];
        self.inp.read_exact(&mut h).map_err(|_| bad("备份文件被截断"))?;
        let (last, len) = (h[0], u32::from_be_bytes(h[1..5].try_into().expect("4")) as usize);
        if last > 1 || !(16..=MAX_CHUNK).contains(&len) {
            return Err(bad("备份文件损坏"));
        }
        let mut ct = vec![0u8; len];
        self.inp.read_exact(&mut ct).map_err(|_| bad("备份文件被截断"))?;
        self.buf = self.cipher.decrypt(Nonce::from_slice(&nonce(self.counter)), Payload { msg: &ct, aad: &[last] }).map_err(|_| bad(WRONG))?;
        self.pos = 0;
        self.counter += 1;
        self.done = last == 1;
        Ok(())
    }
}

impl<R: Read> Read for DecReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.buf.len() {
            if self.done {
                return Ok(0);
            }
            self.next_chunk()?;
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// 文件是否是 EasyMail 加密备份（看文件头）
pub fn is_encrypted(path: &Path) -> bool {
    let mut magic = [0u8; 5];
    path.is_file() && std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && &magic == MAGIC
}

/// 把目录打成 tar 并加密写入 `out`
pub fn pack_encrypted(src: &Path, out: &Path, password: &str) -> Result<()> {
    let f = std::fs::File::create(out).with_context(|| format!("无法创建 {}", out.display()))?;
    let enc = EncWriter::new(io::BufWriter::new(f), password)?;
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all("backup", src)?;
    let enc = tar.into_inner()?;
    enc.finish()?.flush()?;
    Ok(())
}

/// 解密并把整个流读一遍（逐块验证 tag），用来确认文件完整、密码正确
pub fn verify_encrypted(file: &Path, password: &str) -> Result<()> {
    let mut r = DecReader::new(io::BufReader::new(std::fs::File::open(file)?), password)?;
    io::copy(&mut r, &mut io::sink()).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

fn unpack_encrypted(file: &Path, dest: &Path, password: &str) -> Result<()> {
    let r = DecReader::new(io::BufReader::new(std::fs::File::open(file)?), password).map_err(|e| anyhow!("{e}"))?;
    let mut ar = tar::Archive::new(r);
    ar.set_preserve_permissions(true);
    ar.unpack(dest).map_err(|e| anyhow!("{e}"))?;
    // tar 读到结束标记就停：把剩下的读完，确保「末块」也通过验证（防止被截断）
    io::copy(&mut ar.into_inner(), &mut io::sink()).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

/// 加密备份：先在数据目录旁的临时目录里做出普通备份，再打包加密成 `parent/EasyMail-备份-<时间>.embak`。
/// 明文只在应用自己的数据目录旁短暂存在，不会落到用户选的（可能是云盘的）目录里。
pub fn backup_encrypted(store: &Store, parent: &Path, password: &str) -> Result<BackupReport> {
    if password.is_empty() {
        bail!("密码不能为空");
    }
    let tmp = store.dir().with_file_name("backup-tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    let result = (|| -> Result<BackupReport> {
        let mut rep = archive::backup(store, &tmp)?;
        std::fs::create_dir_all(parent)?;
        let file = parent.join(format!("EasyMail-备份-{}.{EXT}", transfer::stamp(now_ms())));
        pack_encrypted(&rep.dir, &file, password)?;
        if let Err(e) = verify_encrypted(&file, password) {
            let _ = std::fs::remove_file(&file);
            return Err(e.context("加密备份写入后校验失败"));
        }
        rep.bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        rep.dir = file;
        Ok(rep)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

// ---------------- 恢复 ----------------

/// 待恢复的数据（已校验）：概览，给用户确认用
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StageInfo {
    pub accounts: i64,
    pub messages: i64,
    pub with_body: i64,
}

pub fn pending_dir(data_dir: &Path) -> PathBuf {
    data_dir.with_file_name("restore-pending")
}

/// 把备份（目录或 .embak 文件）校验后放进暂存目录，等下次启动时切换。失败不留任何痕迹。
pub fn stage_restore(source: &Path, password: Option<&str>, data_dir: &Path) -> Result<StageInfo> {
    let pending = pending_dir(data_dir);
    let _ = std::fs::remove_dir_all(&pending);
    let r = stage_inner(source, password, &pending);
    if r.is_err() {
        let _ = std::fs::remove_dir_all(&pending);
    }
    r
}

fn stage_inner(source: &Path, password: Option<&str>, pending: &Path) -> Result<StageInfo> {
    if source.is_file() {
        if !is_encrypted(source) {
            bail!("这不是 EasyMail 备份（需要备份目录，或 .{EXT} 加密备份文件）");
        }
        let raw = pending.with_file_name("restore-unpack");
        let _ = std::fs::remove_dir_all(&raw);
        let res = (|| -> Result<()> {
            unpack_encrypted(source, &raw, password.ok_or_else(|| anyhow!("需要密码"))?)?;
            let root = raw.join("backup");
            if !root.join(DB_NAME).exists() {
                bail!("备份里没有数据库，文件可能不是 EasyMail 备份");
            }
            std::fs::rename(&root, pending)?;
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&raw);
        res?;
    } else {
        if !source.join(DB_NAME).exists() {
            bail!("所选目录里没有 {DB_NAME}。请选择备份目录本身（名为 EasyMail-备份-…）或 .{EXT} 文件");
        }
        let (mut bytes, mut n) = (0, 0);
        archive::copy_dir(source, pending, &mut bytes, &mut n)?;
        // 备份目录里的 easymail.db 也要一起拷（copy_dir 已包含）；确认暂存里有
        if !pending.join(DB_NAME).exists() {
            bail!("复制备份失败");
        }
    }
    check_staged(pending)
}

fn check_staged(dir: &Path) -> Result<StageInfo> {
    if !dir.join("master.key").exists() {
        bail!("备份里没有 master.key，恢复后无法解密账号密码，已取消");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("master.key"), std::fs::Permissions::from_mode(0o600))?;
    }
    let c = rusqlite::Connection::open_with_flags(dir.join(DB_NAME), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).context("无法打开备份里的数据库")?;
    let qc: String = c.query_row("PRAGMA quick_check", [], |r| r.get(0)).context("备份里的数据库损坏")?;
    if qc != "ok" {
        bail!("备份里的数据库未通过完整性检查：{qc}");
    }
    let ver: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if ver as usize > MIGRATIONS.len() {
        bail!("这份备份来自更新版本的 EasyMail（数据库版本 {ver}），请先升级应用");
    }
    let count = |sql: &str| -> Result<i64> { Ok(c.query_row(sql, [], |r| r.get(0))?) };
    let info = StageInfo {
        accounts: count("SELECT COUNT(*) FROM accounts WHERE provider != 'local'")?,
        messages: count("SELECT COUNT(*) FROM messages")?,
        with_body: count("SELECT COUNT(*) FROM messages WHERE body_state = 1")?,
    };
    Ok(info)
}

/// 启动最早期调用：若有暂存的恢复数据，就把旧数据目录改名保留、换上暂存目录。
/// 返回保留下来的旧数据目录。任何一步失败都会回滚到原状。
pub fn apply_pending(data_dir: &Path) -> Result<Option<PathBuf>> {
    let pending = pending_dir(data_dir);
    if !pending.join(DB_NAME).exists() {
        return Ok(None);
    }
    let old = data_dir.with_file_name(format!("data.before-restore-{}", transfer::stamp(now_ms())));
    let had_old = data_dir.exists();
    if had_old {
        std::fs::rename(data_dir, &old).context("无法保留旧数据目录")?;
    }
    if let Err(e) = std::fs::rename(&pending, data_dir) {
        if had_old {
            let _ = std::fs::rename(&old, data_dir);
        }
        return Err(anyhow!(e).context("无法切换到恢复的数据"));
    }
    Ok(had_old.then_some(old))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(dir: &Path) -> Store {
        let s = Store::open(&dir.join("data")).unwrap();
        crate::core::queries::tests::seed(&s);
        crate::core::messages::store_body(&s, 1, b"From: a@b.c\r\nSubject: x\r\n\r\nhello\r\n").unwrap();
        s
    }

    #[test]
    fn stream_roundtrip_tamper_and_wrong_password() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        // 大于一个分块，确保跨块
        let big: Vec<u8> = (0..(CHUNK * 2 + 123)).map(|i| (i % 251) as u8).collect();
        std::fs::write(src.join("sub/big.bin"), &big).unwrap();
        std::fs::write(src.join("a.txt"), "你好").unwrap();
        let f = dir.path().join("x.embak");
        pack_encrypted(&src, &f, "pw-1").unwrap();
        assert!(is_encrypted(&f));
        verify_encrypted(&f, "pw-1").unwrap();
        assert!(verify_encrypted(&f, "pw-2").unwrap_err().to_string().contains("密码错误"));
        let out = dir.path().join("out");
        unpack_encrypted(&f, &out, "pw-1").unwrap();
        assert_eq!(std::fs::read(out.join("backup/sub/big.bin")).unwrap(), big);
        assert_eq!(std::fs::read_to_string(out.join("backup/a.txt")).unwrap(), "你好");
        // 篡改一个字节
        let mut raw = std::fs::read(&f).unwrap();
        let mid = raw.len() / 2;
        raw[mid] ^= 1;
        std::fs::write(dir.path().join("t.embak"), &raw).unwrap();
        assert!(verify_encrypted(&dir.path().join("t.embak"), "pw-1").is_err());
        // 截断（去掉最后一块）
        let raw = std::fs::read(&f).unwrap();
        std::fs::write(dir.path().join("c.embak"), &raw[..raw.len() - 40]).unwrap();
        assert!(verify_encrypted(&dir.path().join("c.embak"), "pw-1").is_err());
        assert!(!is_encrypted(&src.join("a.txt")));
    }

    #[test]
    fn encrypted_backup_then_restore_swaps_data() {
        let dir = tempfile::tempdir().unwrap();
        let s = seeded(dir.path());
        let out = dir.path().join("bk");
        let rep = backup_encrypted(&s, &out, "secret").unwrap();
        assert!(rep.verified && rep.dir.extension().unwrap() == EXT, "{:?}", rep);
        assert!(!dir.path().join("backup-tmp").exists(), "明文临时目录要清掉");
        // 备份文件里搜不到明文
        let raw = std::fs::read(&rep.dir).unwrap();
        assert!(!raw.windows(5).any(|w| w == b"hello"));
        drop(s);

        let data = dir.path().join("data");
        assert!(stage_restore(&rep.dir, Some("wrong"), &data).is_err());
        assert!(!pending_dir(&data).exists(), "失败不留暂存");
        let info = stage_restore(&rep.dir, Some("secret"), &data).unwrap();
        assert_eq!((info.messages, info.with_body), (5, 1));

        // 在当前数据里多放一个标记文件，切换后应该在 before-restore 里
        std::fs::write(data.join("marker"), "old").unwrap();
        let old = apply_pending(&data).unwrap().expect("旧目录被保留");
        assert!(old.join("marker").exists());
        assert!(!data.join("marker").exists());
        assert!(!pending_dir(&data).exists());
        let s2 = Store::open(&data).unwrap();
        let n: i64 = s2.r().query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 5);
        assert!(apply_pending(&data).unwrap().is_none());
    }

    #[test]
    fn plain_backup_dir_restores_and_bad_source_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let s = seeded(dir.path());
        let rep = archive::backup(&s, &dir.path().join("bk")).unwrap();
        drop(s);
        let data = dir.path().join("data");
        // 选了上一层目录：给出明确提示
        assert!(stage_restore(&dir.path().join("bk"), None, &data).unwrap_err().to_string().contains("备份目录本身"));
        let info = stage_restore(&rep.dir, None, &data).unwrap();
        assert_eq!(info.messages, 5);
        // 缺少 master.key 的备份被拒绝
        std::fs::remove_dir_all(pending_dir(&data)).unwrap();
        let broken = dir.path().join("broken");
        archive::copy_dir(&rep.dir, &broken, &mut 0, &mut 0).unwrap();
        std::fs::remove_file(broken.join("master.key")).unwrap();
        assert!(stage_restore(&broken, None, &data).unwrap_err().to_string().contains("master.key"));
        assert!(!pending_dir(&data).exists());
        // 普通文件不是备份
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        assert!(stage_restore(&dir.path().join("a.txt"), None, &data).is_err());
    }
}
