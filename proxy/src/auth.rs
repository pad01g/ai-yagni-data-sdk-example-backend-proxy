//! 顧客の認証。パスワードの照合とセッションの発行・検証は proxy がする（鍵とハッシュを proxy の外へ出さない）。
//!
//! - パスワード: argon2id（m=19 MiB, t=2, p=1）。重い計算なので、同時に走らせる数を CPU の数までにし、
//!   空きを少し待っても空かなければ混んでいると返す（ログインの連打で proxy が止まらないように）。
//! - セッションのトークン: `顧客 ID.世代.発行時刻.署名`。署名は HMAC-SHA256。
//!   世代（customers.session_epoch）が今の世代と違えば無効（ログアウトで世代を上げる）。
//!   期限の判定は Verus で証明した `session_is_fresh` で行う。
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::sync::Semaphore;

pub const MIN_PASSWORD: usize = 8;
pub const MAX_PASSWORD: usize = 128;
/// 鍵の最低の長さ（バイト）。
pub const MIN_SECRET: usize = 32;

fn argon2() -> Argon2<'static> {
    let params = Params::new(19 * 1024, 2, 1, None).expect("argon2 params");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// 登録のないメールでも、照合と同じだけ時間をかけるためのハッシュ（登録の有無を応答時間で漏らさない）。
static DUMMY: LazyLock<String> = LazyLock::new(|| hash_now("dummy password for unknown emails"));

fn hash_now(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    argon2().hash_password(password.as_bytes(), &salt).expect("argon2 hash").to_string()
}

fn verify_now(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| argon2().verify_password(password.as_bytes(), &h).is_ok())
}

/// argon2 を走らせる場所。同時に走らせる数を絞る。
#[derive(Clone)]
pub struct Passwords {
    slots: Arc<Semaphore>,
    wait: Duration,
}

/// 混んでいて計算できなかった。
#[derive(Debug)]
pub struct Busy;

impl Passwords {
    pub fn new(parallel: usize, wait: Duration) -> Self {
        LazyLock::force(&DUMMY);
        Self { slots: Arc::new(Semaphore::new(parallel.max(1))), wait }
    }

    async fn run<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> Result<T, Busy> {
        let _permit =
            tokio::time::timeout(self.wait, self.slots.acquire()).await.map_err(|_| Busy)?.map_err(|_| Busy)?;
        tokio::task::spawn_blocking(f).await.map_err(|_| Busy)
    }

    pub async fn hash(&self, password: String) -> Result<String, Busy> {
        self.run(move || hash_now(&password)).await
    }

    /// hash が None（登録がない）でも、同じだけ計算してから false を返す。
    pub async fn verify(&self, password: String, hash: Option<String>) -> Result<bool, Busy> {
        self.run(move || {
            let known = hash.is_some();
            let ok = verify_now(&password, hash.as_deref().unwrap_or(&DUMMY));
            known && ok
        })
        .await
    }
}

/// 検証できたセッション。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim {
    pub customer_id: u64,
    pub epoch: u32,
}

pub struct Sessions {
    key: Vec<u8>,
    /// 有効な秒数
    pub ttl: u64,
}

fn mac(key: &[u8]) -> Hmac<Sha256> {
    Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length")
}

impl Sessions {
    /// 鍵が短すぎれば Err（推測できる鍵でセッションを偽造されないように）。
    pub fn new(key: &[u8], ttl: u64) -> Result<Self, String> {
        if key.len() < MIN_SECRET {
            return Err(format!("the session secret must be at least {MIN_SECRET} bytes"));
        }
        Ok(Self { key: key.to_vec(), ttl })
    }

    pub fn issue(&self, claim: Claim, now: u64) -> String {
        let payload = format!("{}.{}.{now}", claim.customer_id, claim.epoch);
        let mut m = mac(&self.key);
        m.update(payload.as_bytes());
        format!("{payload}.{}", URL_SAFE_NO_PAD.encode(m.finalize().into_bytes()))
    }

    /// 署名が合い、期限内ならその中身（世代が今のものかは呼ぶ側が DB と比べる）。
    pub fn verify(&self, token: &str, now: u64) -> Option<Claim> {
        if token.len() > 128 {
            return None;
        }
        let (payload, sig) = token.rsplit_once('.')?;
        let sig = URL_SAFE_NO_PAD.decode(sig).ok()?;
        let mut m = mac(&self.key);
        m.update(payload.as_bytes());
        m.verify_slice(&sig).ok()?;
        let mut parts = payload.split('.');
        let customer_id = parts.next()?.parse().ok()?;
        let epoch = parts.next()?.parse().ok()?;
        let issued = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        proxy_verified::session_is_fresh(issued, now, self.ttl).then_some(Claim { customer_id, epoch })
    }
}

/// メールを比べられる形にする（前後の空白を除き、小文字にする）。形が正しくなければ None。
pub fn normalize_email(email: &str) -> Option<String> {
    let e = email.trim().to_lowercase();
    let (local, domain) = e.split_once('@')?;
    let ok = (3..=254).contains(&e.len())
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && !e.chars().any(|c| c.is_whitespace() || c.is_control());
    ok.then_some(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn tokens_are_signed_and_expire() {
        let s = Sessions::new(KEY, 100).unwrap();
        let claim = Claim { customer_id: 7, epoch: 3 };
        let t = s.issue(claim, 1000);
        assert_eq!(s.verify(&t, 1050), Some(claim));
        assert_eq!(s.verify(&t, 1100), None);
        assert_eq!(s.verify(&t.replacen('7', "8", 1), 1050), None);
        assert_eq!(s.verify(&t.replacen(".3.", ".4.", 1), 1050), None);
        let other = Sessions::new(b"another key that is long enough!!", 100).unwrap();
        assert_eq!(other.verify(&t, 1050), None);
    }

    #[test]
    fn short_secrets_are_refused() {
        assert!(Sessions::new(b"local-development-only", 100).is_err());
    }

    #[test]
    fn emails_are_normalized() {
        assert_eq!(normalize_email("  Hanako@Example.COM "), Some("hanako@example.com".to_owned()));
        assert_eq!(normalize_email("@"), None);
        assert_eq!(normalize_email("a@b"), None);
        assert_eq!(normalize_email("a b@example.com"), None);
        assert_eq!(normalize_email("a@@example.com"), None);
    }

    #[tokio::test]
    async fn passwords_verify_and_unknown_emails_fail() {
        let p = Passwords::new(2, Duration::from_secs(5));
        let h = p.hash("correct horse".to_owned()).await.unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(p.verify("correct horse".to_owned(), Some(h.clone())).await.unwrap());
        assert!(!p.verify("wrong".to_owned(), Some(h)).await.unwrap());
        assert!(!p.verify("dummy password for unknown emails".to_owned(), None).await.unwrap());
    }

    #[test]
    fn the_fixture_hash_is_password() {
        let fixture =
            "$argon2id$v=19$m=19456,t=2,p=1$PbhbkLnrUMnhKKg4a8PJQA$OKejesFcn3ysRMQpWTdswoa1FwmNYbXLpbwbaHCuK8A";
        assert!(verify_now("password", fixture));
    }
}
