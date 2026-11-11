//! 凭据加密：AES-256-GCM，主密钥存在 `<数据目录>/master.key`（base64，权限 0600，首次启动生成）。
//! 密文格式与 just-mail 完全一致：`v1:<iv b64>:<tag b64>:<密文 b64>`，旧数据目录可直接沿用。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, bail, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rand::RngCore;
use std::path::Path;

#[derive(Clone)]
pub struct Crypto {
    key: [u8; 32],
}

impl Crypto {
    pub fn from_key(key: [u8; 32]) -> Self {
        Crypto { key }
    }

    /// 读取主密钥；不存在就生成一个（0600）。
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            let raw = std::fs::read_to_string(path)?;
            let bytes = B64.decode(raw.trim()).map_err(|_| anyhow!("master.key 内容损坏"))?;
            let key: [u8; 32] = bytes.try_into().map_err(|_| anyhow!("master.key 长度不对"))?;
            return Ok(Crypto { key });
        }
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        // 一步以 0600 创建（不是先按默认权限写、再 chmod，中间没有可被读到的窗口）；create_new 防止覆盖并发写入的密钥
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        use std::io::Write;
        opts.open(path)?.write_all(B64.encode(key).as_bytes())?;
        Ok(Crypto { key })
    }

    pub fn encrypt(&self, plain: &str) -> String {
        let cipher = Aes256Gcm::new_from_slice(&self.key).expect("key");
        let mut iv = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut iv);
        let mut out = cipher.encrypt(Nonce::from_slice(&iv), plain.as_bytes()).expect("encrypt");
        // aes-gcm 把 16 字节 tag 追加在密文末尾；Node 的格式是分开存放
        let tag = out.split_off(out.len() - 16);
        format!("v1:{}:{}:{}", B64.encode(iv), B64.encode(tag), B64.encode(out))
    }

    pub fn decrypt(&self, blob: &str) -> Result<String> {
        let parts: Vec<&str> = blob.split(':').collect();
        if parts.len() != 4 || parts[0] != "v1" {
            bail!("凭据格式无法识别");
        }
        let iv = B64.decode(parts[1])?;
        let tag = B64.decode(parts[2])?;
        let mut data = B64.decode(parts[3])?;
        if iv.len() != 12 || tag.len() != 16 {
            bail!("凭据格式无法识别");
        }
        data.extend_from_slice(&tag);
        let cipher = Aes256Gcm::new_from_slice(&self.key).expect("key");
        let plain = cipher.decrypt(Nonce::from_slice(&iv), data.as_ref()).map_err(|_| anyhow!("凭据解密失败：master.key 与数据库不匹配"))?;
        Ok(String::from_utf8(plain)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_tamper() {
        let c = Crypto::from_key([7u8; 32]);
        let blob = c.encrypt("abcd efgh 授权码");
        assert!(blob.starts_with("v1:"));
        assert_eq!(c.decrypt(&blob).unwrap(), "abcd efgh 授权码");
        // 篡改密文 → 认证失败
        let mut parts: Vec<String> = blob.split(':').map(String::from).collect();
        parts[3] = B64.encode(b"xxxxxxxxxxxxxxxxx");
        assert!(c.decrypt(&parts.join(":")).is_err());
        assert!(c.decrypt("v2:a:b:c").is_err());
        // 换密钥 → 失败
        assert!(Crypto::from_key([8u8; 32]).decrypt(&blob).is_err());
    }

    #[test]
    fn key_file_is_private_and_stable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("master.key");
        let a = Crypto::load_or_create(&p).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let b = Crypto::load_or_create(&p).unwrap();
        assert_eq!(b.decrypt(&a.encrypt("pw")).unwrap(), "pw");
    }

    #[test]
    fn empty_plaintext() {
        let c = Crypto::from_key([1u8; 32]);
        assert_eq!(c.decrypt(&c.encrypt("")).unwrap(), "");
    }

    /// Node 端 crypto.createCipheriv('aes-256-gcm') 生成的固定样本（key 全 0x11，iv 全 0x22）
    #[test]
    fn decrypts_node_format() {
        let c = Crypto::from_key([0x11u8; 32]);
        let blob = "v1:IiIiIiIiIiIiIiIi:RaTmKR61T8XuaSan45Rk1w==:8XmPr11MeP9kH79eK8I=";
        assert_eq!(c.decrypt(blob).unwrap(), "授权码 abcd");
    }
}
