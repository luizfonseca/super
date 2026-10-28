//! A coarse clock, for what's measured in seconds (how long an upstream's connection was idle):
//! cheaper to read than the precise one, which every request reads already. On Linux, the
//! kernel's last tick: a few milliseconds behind at most.

use std::time::Duration;

/// A moment, in milliseconds on a monotonic clock
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Coarse(u64);

impl Coarse {
    /// Now, on the coarse clock
    pub fn now() -> Self {
        Self(millis())
    }

    /// `duration` later
    #[must_use]
    pub fn after(self, duration: Duration) -> Self {
        let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        Self(self.0.saturating_add(millis))
    }

    /// How long after `earlier` this is (none, when it's before)
    pub fn since(self, earlier: Self) -> Duration {
        Duration::from_millis(self.0.saturating_sub(earlier.0))
    }
}

#[cfg(target_os = "linux")]
fn millis() -> u64 {
    use nix::time::{clock_gettime, ClockId};

    clock_gettime(ClockId::CLOCK_MONOTONIC_COARSE).map_or_else(
        |_| precise_millis(),
        |now| {
            let seconds = u64::try_from(now.tv_sec()).unwrap_or_default();
            let nanos = u64::try_from(now.tv_nsec()).unwrap_or_default();
            seconds.saturating_mul(1000).saturating_add(nanos / 1_000_000)
        },
    )
}

#[cfg(not(target_os = "linux"))]
fn millis() -> u64 {
    precise_millis()
}

/// Since the clock was first read, on the precise clock
fn precise_millis() -> u64 {
    static START: std::sync::LazyLock<std::time::Instant> =
        std::sync::LazyLock::new(std::time::Instant::now);
    u64::try_from(START.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::Coarse;

    #[test]
    fn test_coarse_moments() {
        let now = Coarse::now();
        let later = now.after(Duration::from_secs(5));
        assert!(later > now);
        assert_eq!(later.since(now), Duration::from_secs(5));
        assert_eq!(now.since(later), Duration::ZERO);
        // It goes forward, as the precise clock does
        std::thread::sleep(Duration::from_millis(30));
        assert!(Coarse::now().since(now) >= Duration::from_millis(15));
    }
}
