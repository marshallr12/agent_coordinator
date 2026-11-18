//! The helper's mint budget: a token bucket bounding how often one helper
//! asks GitHub for an installation token, so a sandbox that loops on
//! well-formed requests cannot spend the push App's shared rate limit.
use std::time::{Duration, Instant};

/// Mints allowed back to back: room for a push and a couple of retries.
pub const MINT_BURST: u32 = 3;
/// One mint is earned back per interval once the burst is spent: a looping
/// sandbox gets one mint per interval, while an implementer pushing a
/// checkpoint every few minutes never waits.
pub const MINT_INTERVAL: Duration = Duration::from_secs(20);

/// A token bucket of mints.
#[derive(Debug)]
pub struct MintBudget {
    available: u32,
    burst: u32,
    interval: Duration,
    refilled_at: Instant,
}

impl MintBudget {
    /// A full bucket of `burst` mints, earning one back per `interval`.
    pub fn new(burst: u32, interval: Duration, now: Instant) -> Self {
        Self {
            available: burst,
            burst,
            interval,
            refilled_at: now,
        }
    }

    /// Spends one mint if the bucket, refilled up to `now`, holds one.
    pub fn take(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.available == 0 {
            return false;
        }
        self.available -= 1;
        true
    }

    /// Adds the mints earned since the last refill, never above the burst;
    /// a full bucket restarts its clock so idle time is not banked.
    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.refilled_at);
        let earned = elapsed.as_nanos() / self.interval.as_nanos().max(1);
        let total = u128::from(self.available) + earned;
        if total >= u128::from(self.burst) {
            self.available = self.burst;
            self.refilled_at = now;
        } else if earned > 0 {
            self.available = u32::try_from(total).unwrap_or(self.burst);
            self.refilled_at += self.interval * u32::try_from(earned).unwrap_or(u32::MAX);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_burst_is_spent_then_one_mint_returns_per_interval() {
        let start = Instant::now();
        let mut budget = MintBudget::new(3, Duration::from_secs(20), start);
        assert!((0..3).all(|_| budget.take(start)));
        assert!(!budget.take(start));
        assert!(!budget.take(start + Duration::from_secs(19)));
        assert!(budget.take(start + Duration::from_secs(20)));
        assert!(!budget.take(start + Duration::from_secs(39)));
        assert!(budget.take(start + Duration::from_secs(40)));
    }

    #[test]
    fn idle_time_refills_only_up_to_the_burst() {
        let start = Instant::now();
        let mut budget = MintBudget::new(3, Duration::from_secs(20), start);
        assert!((0..3).all(|_| budget.take(start)));
        let later = start + Duration::from_secs(3600);
        assert!((0..3).all(|_| budget.take(later)));
        assert!(!budget.take(later));
    }
}
