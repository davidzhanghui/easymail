//! 应用内自动更新：下载新版 dmg → 校验签名与哈希 → 取出新 .app → 退出后替换并重启。
//!
//! 信任链（应用没有 Apple 公证，所以不能只靠 Gatekeeper）：
//!   发布时用私钥给 `SHA256SUMS.txt` 做 Ed25519 签名（`SHA256SUMS.txt.sig`），公钥内置在本文件；
//!   更新时先验签 SHA256SUMS.txt，再用其中的哈希校验 dmg。GitHub 账号被盗、CDN 被换包都过不了这一关。
//!
//! 应用自己下载的文件不会带 `com.apple.quarantine`，所以替换后的新版不会再被 Gatekeeper 拦；
//! 替换脚本仍会清一次隔离标记，以防万一。

use super::net::Proxy;
use super::tools::{self, Release};
use anyhow::{anyhow, bail, Context, Result};
use ring::signature;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

/// 更新签名公钥（Ed25519，32 字节，十六进制）。对应私钥只在发布流水线的 Secrets 里（UPDATE_SIGNING_KEY）
pub const UPDATE_PUBKEY_HEX: &str = "9bffd6f4a51eee7c1918010313bdc4e9ddcdb63db3b1b6cd26893f2b2786d9a8";

pub const SUMS_NAME: &str = "SHA256SUMS.txt";
pub const SIG_NAME: &str = "SHA256SUMS.txt.sig";
const MAX_DMG: u64 = 400 * 1024 * 1024;

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 验证 `sums` 的 Ed25519 签名
pub fn verify_sums(pubkey: &[u8], sums: &[u8], sig: &[u8]) -> Result<()> {
    signature::UnparsedPublicKey::new(&signature::ED25519, pubkey).verify(sums, sig).map_err(|_| anyhow!("更新包签名校验失败：已拒绝安装"))
}

/// 在 `shasum -a 256` 格式的清单里找某个文件的哈希
pub fn expected_hash(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (h, name) = l.split_once(char::is_whitespace)?;
        let name = name.trim().trim_start_matches('*');
        (name == file && h.len() == 64).then(|| h.to_ascii_lowercase())
    })
}

pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex_encode(&h.finalize()))
}

/// 当前正在运行的 .app 路径；不是从 .app 里启动的（cargo run 等）返回 None
pub fn installed_app() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension().is_some_and(|e| e == "app")).then(|| app.to_path_buf())
}

/// 这个版本能否自动更新：在 .app 里运行，且发布里带了 dmg、清单和签名
pub fn can_self_update(rel: &Release) -> bool {
    installed_app().is_some() && rel.asset(&dmg_name(rel)).is_some() && rel.asset(SUMS_NAME).is_some() && rel.asset(SIG_NAME).is_some()
}

pub fn dmg_name(rel: &Release) -> String {
    format!("EasyMail-{}.dmg", rel.version)
}

/// 已校验、已就位到「安装目录旁边」的新版 .app，等待替换
#[derive(Debug)]
pub struct Staged {
    pub app: PathBuf,
    pub staged: PathBuf,
}

/// 下载并校验，返回待替换的新版 .app。全程不碰正在运行的版本
pub async fn prepare(rel: &Release, proxy: Option<&Proxy>, proxy_url: &str) -> Result<Staged> {
    let app = installed_app().ok_or_else(|| anyhow!("当前不是从 .app 运行的，无法自动更新"))?;
    let (dmg_url, sums_url, sig_url) = (
        rel.asset(&dmg_name(rel)).ok_or_else(|| anyhow!("发布里没有 {}", dmg_name(rel)))?,
        rel.asset(SUMS_NAME).ok_or_else(|| anyhow!("发布里没有 {SUMS_NAME}"))?,
        rel.asset(SIG_NAME).ok_or_else(|| anyhow!("发布里没有 {SIG_NAME}（这个版本没有签名，不能自动更新）"))?,
    );
    let get = |u: String| async move {
        let r = super::http::get(&u, proxy).await?;
        if !r.ok() {
            bail!("下载失败：HTTP {}（{u}）", r.status);
        }
        Ok::<_, anyhow::Error>(r.body)
    };
    let (sums, sig) = (get(sums_url.to_string()).await?, get(sig_url.to_string()).await?);
    let pubkey = hex_decode(UPDATE_PUBKEY_HEX).ok_or_else(|| anyhow!("内置公钥不正确"))?;
    verify_sums(&pubkey, &sums, &sig)?;
    let want = expected_hash(&String::from_utf8_lossy(&sums), &dmg_name(rel)).ok_or_else(|| anyhow!("签名清单里没有 {}", dmg_name(rel)))?;

    let (rel, dmg_url, proxy_url) = (rel.clone(), dmg_url.to_string(), proxy_url.to_string());
    tokio::task::spawn_blocking(move || prepare_blocking(&rel, &dmg_url, &want, &proxy_url, app)).await.context("更新任务异常中止")?
}

fn work_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("app.easymail").join("update")
}

fn run(cmd: &mut Command, what: &str) -> Result<String> {
    let o = cmd.stdin(Stdio::null()).output().with_context(|| format!("{what}：无法执行"))?;
    if !o.status.success() {
        bail!("{what}失败：{}", String::from_utf8_lossy(&o.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// 挂载的 dmg，离开作用域自动卸载
struct Mount(PathBuf);

impl Drop for Mount {
    fn drop(&mut self) {
        let _ = Command::new("/usr/bin/hdiutil").args(["detach", "-force", "-quiet"]).arg(&self.0).stdin(Stdio::null()).output();
    }
}

fn plist_value(app: &Path, key: &str) -> Result<String> {
    run(Command::new("/usr/bin/defaults").arg("read").arg(app.join("Contents/Info")).arg(key), "读取 Info.plist")
}

fn prepare_blocking(rel: &Release, dmg_url: &str, want: &str, proxy_url: &str, app: PathBuf) -> Result<Staged> {
    let dir = work_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let dmg = dir.join(dmg_name(rel));

    // 用系统自带的 curl 下载：自动跟随重定向、支持 socks5 / http 代理，且不会给文件打隔离标记
    let mut curl = Command::new("/usr/bin/curl");
    curl.args(["-fsSL", "--proto", "=https", "--proto-redir", "=https", "--retry", "2", "--max-time", "1800", "--max-filesize"]).arg(MAX_DMG.to_string());
    if !proxy_url.trim().is_empty() {
        curl.arg("-x").arg(proxy_url.trim().replacen("socks5://", "socks5h://", 1));
    }
    run(curl.arg("-o").arg(&dmg).arg(dmg_url), "下载更新包")?;
    if sha256_file(&dmg)? != want {
        bail!("更新包哈希与签名清单不一致，已丢弃");
    }

    let mnt = dir.join("mnt");
    std::fs::create_dir_all(&mnt)?;
    run(Command::new("/usr/bin/hdiutil").args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-quiet", "-mountpoint"]).arg(&mnt).arg(&dmg), "挂载更新包")?;
    let _mount = Mount(mnt.clone());
    let src =
        std::fs::read_dir(&mnt)?.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "app")).ok_or_else(|| anyhow!("更新包里没有 .app"))?;

    // 放在安装目录旁边：同一个卷，之后的替换是原子改名；没有写权限会在这里报错
    let name = app.file_name().ok_or_else(|| anyhow!("安装路径不正确"))?.to_string_lossy().to_string();
    let staged = app.with_file_name(format!(".{name}.update"));
    let _ = std::fs::remove_dir_all(&staged);
    run(Command::new("/usr/bin/ditto").arg(&src).arg(&staged), "复制新版本（安装目录可能没有写入权限）")?;
    drop(_mount);

    let check = || -> Result<()> {
        run(Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(&staged), "新版本签名校验")?;
        let (id_new, id_old) = (plist_value(&staged, "CFBundleIdentifier")?, plist_value(&app, "CFBundleIdentifier")?);
        if id_new != id_old {
            bail!("新版本的 Bundle ID（{id_new}）与当前（{id_old}）不一致");
        }
        let v = plist_value(&staged, "CFBundleShortVersionString")?;
        if v != rel.version || !tools::newer(&v, super::config::VERSION) {
            bail!("新版本号 {v} 与发布（{}）不符，或不比当前版本新", rel.version);
        }
        Ok(())
    };
    if let Err(e) = check() {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(Staged { app, staged })
}

/// 等旧进程退出 → 旧版移进废纸篓 → 新版就位 → 清隔离标记 → 启动。新版就位失败会还原旧版
const SWAP_SCRIPT: &str = r#"
pid="$1"; app="$2"; new="$3"
i=0
while kill -0 "$pid" 2>/dev/null && [ "$i" -lt 150 ]; do sleep 0.2; i=$((i+1)); done
old="${app}.old.$$"
if mv "$app" "$old" && mv "$new" "$app"; then
  xattr -dr com.apple.quarantine "$app" 2>/dev/null
  trash="${TRASH_DIR:-$HOME/.Trash}"
  mv "$old" "$trash/$(basename "$app").$(date +%Y%m%d%H%M%S)" 2>/dev/null || rm -rf "$old"
else
  [ -e "$old" ] && [ ! -e "$app" ] && mv "$old" "$app"
  rm -rf "$new"
fi
${OPEN_CMD:-open} "$app"
"#;

static DOWNLOAD_BUSY: AtomicBool = AtomicBool::new(false);

/// 同时只允许一个下载。已经在下载时返回 false
pub fn begin_download() -> bool {
    DOWNLOAD_BUSY.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok()
}

pub fn end_download() {
    DOWNLOAD_BUSY.store(false, Ordering::SeqCst);
}

/// 已下载、已校验、放在安装目录旁边、等重启替换的新版本
pub fn pending_version() -> Option<String> {
    let app = installed_app()?;
    let name = app.file_name()?.to_str()?;
    let staged = app.with_file_name(format!(".{name}.update"));
    if !staged.join("Contents/Info.plist").is_file() {
        return None;
    }
    let version = plist_value(&staged, "CFBundleShortVersionString").ok()?;
    tools::newer(&version, super::config::VERSION).then_some(version)
}

/// 退出当前进程之前调用：用旁边那个已校验的新版本替换并重新打开
pub fn restart_pending() -> Result<()> {
    let app = installed_app().ok_or_else(|| anyhow!("当前不是从 .app 运行的，无法自动更新"))?;
    let name = app.file_name().and_then(|n| n.to_str()).ok_or_else(|| anyhow!("安装路径不正确"))?;
    let staged = app.with_file_name(format!(".{name}.update"));
    if pending_version().is_none() {
        bail!("没有已下载的更新");
    }
    relaunch(&Staged { app, staged })
}

/// 启动脱离本进程的替换脚本；调用后应立即退出应用
pub fn relaunch(s: &Staged) -> Result<()> {
    use std::os::unix::process::CommandExt;
    Command::new("/bin/sh")
        .arg("-c")
        .arg(SWAP_SCRIPT)
        .arg("sh")
        .arg(std::process::id().to_string())
        .arg(&s.app)
        .arg(&s.staged)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("启动替换脚本失败")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn keypair() -> Ed25519KeyPair {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
    }

    #[test]
    fn embedded_pubkey_is_32_bytes() {
        assert_eq!(hex_decode(UPDATE_PUBKEY_HEX).unwrap().len(), 32);
    }

    #[test]
    fn signature_roundtrip_and_tamper() {
        let kp = keypair();
        let sums = format!("{}  EasyMail-1.2.3.dmg\n", "ab".repeat(32));
        let sig = kp.sign(sums.as_bytes());
        assert!(verify_sums(kp.public_key().as_ref(), sums.as_bytes(), sig.as_ref()).is_ok());
        // 清单被改过
        let bad = sums.replace("ab", "ac");
        assert!(verify_sums(kp.public_key().as_ref(), bad.as_bytes(), sig.as_ref()).is_err());
        // 换了别人的密钥
        assert!(verify_sums(keypair().public_key().as_ref(), sums.as_bytes(), sig.as_ref()).is_err());
        // 签名被截断
        assert!(verify_sums(kp.public_key().as_ref(), sums.as_bytes(), &sig.as_ref()[..10]).is_err());
    }

    /// 发布流水线用 openssl 签名：这里确认 openssl 产出的签名能被内置公钥验证
    #[test]
    fn openssl_signature_vector() {
        let pubkey = hex_decode("9bffd6f4a51eee7c1918010313bdc4e9ddcdb63db3b1b6cd26893f2b2786d9a8").unwrap();
        // 由 scripts/release-sign.sh 的同一条命令生成；私钥不在仓库里，所以这里只在能找到向量文件时检查
        let (m, s) = (std::env::var("EASYMAIL_SIGN_TEST_MSG"), std::env::var("EASYMAIL_SIGN_TEST_SIG"));
        if let (Ok(m), Ok(s)) = (m, s) {
            verify_sums(&pubkey, &std::fs::read(m).unwrap(), &std::fs::read(s).unwrap()).unwrap();
        }
    }

    #[test]
    fn parses_sums() {
        let h = "0123456789abcdef".repeat(4);
        let sums = format!("{h}  EasyMail-1.0.1.dmg\n{}  other.dmg\n", "f".repeat(64));
        assert_eq!(expected_hash(&sums, "EasyMail-1.0.1.dmg").as_deref(), Some(h.as_str()));
        assert_eq!(expected_hash(&sums, "EasyMail-1.0.2.dmg"), None);
        assert_eq!(expected_hash(&format!("{h} *EasyMail-1.0.1.dmg"), "EasyMail-1.0.1.dmg").as_deref(), Some(h.as_str()));
        assert_eq!(expected_hash("short  EasyMail-1.0.1.dmg", "EasyMail-1.0.1.dmg"), None);
    }

    #[test]
    fn swap_script_replaces_and_rolls_back() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path();
        let trash = p.join("trash");
        std::fs::create_dir_all(&trash).unwrap();
        let mk = |d: &str, v: &str| {
            std::fs::create_dir_all(p.join(d)).unwrap();
            std::fs::write(p.join(d).join("v"), v).unwrap();
        };
        let run = |app: &str, new: &str| {
            let st = Command::new("/bin/sh")
                .arg("-c")
                .arg(SWAP_SCRIPT)
                .arg("sh")
                .arg("999999999") // 不存在的 pid：不用等
                .arg(p.join(app))
                .arg(p.join(new))
                .env("TRASH_DIR", &trash)
                .env("OPEN_CMD", "true")
                .status()
                .unwrap();
            assert!(st.success());
        };
        // 正常替换：新版就位，旧版进废纸篓
        mk("A.app", "old");
        mk(".A.app.update", "new");
        run("A.app", ".A.app.update");
        assert_eq!(std::fs::read_to_string(p.join("A.app/v")).unwrap(), "new");
        assert!(!p.join(".A.app.update").exists());
        assert_eq!(std::fs::read_dir(&trash).unwrap().count(), 1);
        // 新版不存在：旧版保持原样
        run("A.app", ".missing.update");
        assert_eq!(std::fs::read_to_string(p.join("A.app/v")).unwrap(), "new");
    }
}
