//! Client-side request pacing and retry backoff.

use std::sync::Mutex;
use std::time::Duration;
use tokio::time::Instant;

/// Spaces requests evenly: at most `max_per_window` requests per `window`.
#[derive(Debug)]
pub struct RateLimiter {
    interval: Duration,
    next_slot: Mutex<Option<Instant>>,
}

impl RateLimiter {
    pub fn new(max_per_window: u32, window: Duration) -> Self {
        Self {
            interval: window / max_per_window.max(1),
            next_slot: Mutex::new(None),
        }
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Wait until the next request slot is free.
    pub async fn acquire(&self) {
        let wait = {
            let mut next = self.next_slot.lock().expect("rate limiter poisoned");
            let now = Instant::now();
            let slot = next.map_or(now, |n| n.max(now));
            *next = Some(slot + self.interval);
            slot - now
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    /// Push all future slots back, e.g. after a 429 with `Retry-After`.
    pub fn pause_for(&self, delay: Duration) {
        let mut next = self.next_slot.lock().expect("rate limiter poisoned");
        let until = Instant::now() + delay;
        *next = Some(next.map_or(until, |n| n.max(until)));
    }
}

/// Exponential backoff with up to 20 % jitter.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Backoff {
    pub fn delay(&self, attempt: u32) -> Duration {
        let base = self
            .initial
            .saturating_mul(2u32.saturating_pow(attempt.min(20)))
            .min(self.max);
        let jitter = base.mul_f64(fastrand::f64() * 0.2);
        (base + jitter).min(self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn spaces_requests() {
        let limiter = RateLimiter::new(5, Duration::from_secs(10));
        assert_eq!(limiter.interval(), Duration::from_secs(2));
        let start = Instant::now();
        for _ in 0..3 {
            limiter.acquire().await;
        }
        // First is immediate, then two intervals.
        assert_eq!(start.elapsed(), Duration::from_secs(4));
    }

    #[tokio::test(start_paused = true)]
    async fn pause_delays_next_request() {
        let limiter = RateLimiter::new(100, Duration::from_secs(10));
        limiter.acquire().await;
        let start = Instant::now();
        limiter.pause_for(Duration::from_secs(30));
        limiter.acquire().await;
        assert_eq!(start.elapsed(), Duration::from_secs(30));
    }

    #[test]
    fn backoff_grows_and_caps() {
        let b = Backoff {
            initial: Duration::from_millis(500),
            max: Duration::from_secs(10),
        };
        for attempt in 0..10 {
            let d = b.delay(attempt);
            let base = Duration::from_millis(500 * 2u64.pow(attempt)).min(b.max);
            assert!(
                d >= base && d <= b.max.max(base),
                "attempt {attempt}: {d:?}"
            );
            assert!(d <= base.mul_f64(1.2) + Duration::from_millis(1));
        }
        assert_eq!(b.delay(50), b.max);
    }
}
