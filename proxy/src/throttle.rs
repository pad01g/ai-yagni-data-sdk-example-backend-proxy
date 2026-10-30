//! 回数の制限。キー（メール、顧客など）ごとに、窓の時間のあいだの回数を数える（proxy のメモリにだけ置く）。
//!
//! nginx の送り元の IP ごとの制限に加える。IP を替えながらの総当たり（ログイン）や、1 人の顧客が注文を
//! 繰り返して決済サービスや表を埋めることを遅くする。
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 覚えておくキーの数の上限。超えたら、窓を過ぎたものを捨てる（それでも多ければ全部捨てる）。
const MAX_KEYS: usize = 100_000;

pub struct Throttle {
    limit: u32,
    window: Duration,
    seen: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Throttle {
    pub fn new(limit: u32, window: Duration) -> Self {
        Self { limit, window, seen: Mutex::new(HashMap::new()) }
    }

    /// 窓のうちに limit 回に達していれば true（数えない）。
    pub fn exhausted(&self, key: &str) -> bool {
        let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.get(key).is_some_and(|&(since, n)| since.elapsed() < self.window && n >= self.limit)
    }

    /// 1 回数える。
    pub fn count(&self, key: &str) {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if seen.len() >= MAX_KEYS {
            let window = self.window;
            seen.retain(|_, (since, _)| since.elapsed() < window);
            if seen.len() >= MAX_KEYS {
                seen.clear();
            }
        }
        let e = seen.entry(key.to_owned()).or_insert((Instant::now(), 0));
        if e.0.elapsed() >= self.window {
            *e = (Instant::now(), 0);
        }
        e.1 = e.1.saturating_add(1);
    }

    /// 数えて、limit を超えたら true。
    pub fn hit(&self, key: &str) -> bool {
        if self.exhausted(key) {
            return true;
        }
        self.count(key);
        false
    }

    pub fn forget(&self, key: &str) {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_limited_within_the_window() {
        let t = Throttle::new(2, Duration::from_secs(60));
        assert!(!t.hit("a") && !t.hit("a"));
        assert!(t.hit("a"));
        assert!(!t.hit("b"), "other keys are counted apart");
        t.forget("a");
        assert!(!t.hit("a"));
    }

    #[test]
    fn the_window_resets() {
        let t = Throttle::new(1, Duration::from_millis(1));
        assert!(!t.hit("a"));
        std::thread::sleep(Duration::from_millis(5));
        assert!(!t.hit("a"));
    }
}
