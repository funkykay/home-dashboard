use std::time::Duration;

/// Bounded exponential backoff with independent deterministic PRNG state.
#[derive(Debug)]
pub struct Backoff {
    attempt: u32,
    seed: u64,
}
impl Backoff {
    pub fn new(seed: u64) -> Self {
        Self {
            attempt: 0,
            seed: seed.max(1),
        }
    }
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
    pub fn delay(&mut self) -> Duration {
        let cap = (1u64 << self.attempt.min(9)).min(300);
        self.attempt = self.attempt.saturating_add(1);
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        Duration::from_millis(cap * 500 + self.seed % (cap * 500 + 1))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_jitter_and_reset() {
        let mut a = Backoff::new(31);
        let mut b = Backoff::new(45);
        assert_ne!(a.delay(), b.delay());
        for _ in 0..100 {
            assert!(a.delay() <= Duration::from_secs(300));
        }
        a.reset();
        assert!(a.delay() <= Duration::from_secs(1));
    }
}
