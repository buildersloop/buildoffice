//! 呼び出しの回数の制限（仕様の MUST）。
//!
//! 1秒に50回、ためられるのは100回まで（トークンのバケツ。はじめは満杯）。プロセスに1つで、Modern と Legacy をまたいで共有する。

use std::time::Instant;

/// 1秒に足される回数。
pub const RATE_LIMIT_PER_SECOND: u32 = 50;
/// ためられる回数。
pub const RATE_LIMIT_BURST: u32 = 100;

/// 回数の制限（トークンのバケツ）。
#[derive(Debug)]
pub(crate) struct RateLimiter {
    tokens: f64,
    last: Option<Instant>,
}

impl RateLimiter {
    pub(crate) fn new() -> Self {
        Self {
            tokens: f64::from(RATE_LIMIT_BURST),
            last: None,
        }
    }

    /// 1回を数える。まだ使えれば true、超えていれば false（数えない）。
    /// 時計が戻ったときは足さず、最後の時刻も戻さない（戻した分を、あとでもう一度足さないため）。
    pub(crate) fn allow(&mut self, now: Instant) -> bool {
        let now = match self.last {
            Some(last) => {
                let elapsed = now.saturating_duration_since(last).as_secs_f64();
                self.tokens = (self.tokens + elapsed * f64::from(RATE_LIMIT_PER_SECOND))
                    .min(f64::from(RATE_LIMIT_BURST));
                now.max(last)
            }
            None => now,
        };
        self.last = Some(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn burst_then_fifty_per_second() {
        let start = Instant::now();
        let mut limiter = RateLimiter::new();
        assert!((0..100).all(|_| limiter.allow(start)), "はじめに 100 回");
        assert!(!limiter.allow(start), "101 回目は超える");
        let later = start + Duration::from_secs(1);
        assert!(
            (0..50).all(|_| limiter.allow(later)),
            "1秒で 50 回ぶん足される"
        );
        assert!(!limiter.allow(later));
        // 長く空いても、ためられるのは 100 回まで
        let much_later = later + Duration::from_secs(60);
        assert!((0..100).all(|_| limiter.allow(much_later)));
        assert!(!limiter.allow(much_later));
        // 時計が戻っても足さない。最後の時刻も戻さないので、また進んでも、戻った分を足さない
        assert!(!limiter.allow(start));
        assert!(
            !limiter.allow(much_later),
            "戻った分（60 秒）を、もう一度足さない"
        );
        assert!(
            limiter.allow(much_later + Duration::from_millis(20)),
            "進んだ分だけ足す"
        );
    }
}
