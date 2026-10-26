//! A doubling delay between failed sign-ins to one name (#515).
//!
//! Each failure for a name doubles the wait before that name may be tried
//! again, from one second up to [`MAX_DELAY`]; a success clears it. Not a
//! lockout: the account's owner can always get in, at worst after
//! [`MAX_DELAY`], so nobody can lock someone else out by failing on purpose.
//! A name that matches no account is throttled the same way, so the answer
//! cannot be used to tell which names exist.
//!
//! This bounds guessing at one name. Guessing many names from one address
//! is the reverse proxy's to limit (`limit_req` in nginx, documented in
//! `docs/deploy/reverse-proxy.md`): Ridal behind a proxy sees every request
//! come from the proxy.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The longest wait between attempts at one name.
pub const MAX_DELAY: Duration = Duration::from_secs(5 * 60);

/// A name not tried for this long after its wait ended starts again from
/// nothing.
const FORGET_AFTER: Duration = Duration::from_secs(60 * 60);

/// Names remembered at once. Past it, the entry whose wait ended longest
/// ago is dropped, so a flood of made-up names cannot grow the map without
/// bound; that costs a forgotten delay, never a wrongly refused sign-in.
const MAX_ENTRIES: usize = 10_000;

/// Characters of a name kept as its key. Longer than any valid account
/// name, so only made-up names share a key, and they never match anyway.
const MAX_KEY_CHARS: usize = 100;

#[derive(Debug, Clone, Copy)]
struct Entry {
    failures: u32,
    next_allowed: Instant,
}

/// The delays, by submitted name.
#[derive(Debug, Default)]
pub struct LoginThrottle {
    entries: Mutex<HashMap<String, Entry>>,
}

/// The wait after `failures` failures in a row: one second, doubling, at
/// most [`MAX_DELAY`].
fn delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    (Duration::from_secs(1) * 2u32.pow(doublings)).min(MAX_DELAY)
}

fn key(name: &str) -> String {
    name.chars().take(MAX_KEY_CHARS).collect()
}

impl LoginThrottle {
    /// Start an attempt at `name`, or the time left to wait.
    ///
    /// An allowed attempt holds the name as if it had already failed, until
    /// [`Self::failed`] or [`Self::succeeded`] says how it went, so attempts
    /// sent all at once are spaced out like attempts sent one by one.
    pub fn begin(&self, name: &str, now: Instant) -> Result<(), Duration> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let key = key(name);
        let failures = match entries.get(&key) {
            Some(entry) if now < entry.next_allowed => return Err(entry.next_allowed - now),
            Some(entry) if now < entry.next_allowed + FORGET_AFTER => entry.failures,
            _ => 0,
        };
        if entries.len() >= MAX_ENTRIES && !entries.contains_key(&key) {
            make_room(&mut entries, now);
        }
        entries.insert(
            key,
            Entry {
                failures,
                next_allowed: now + delay(failures + 1),
            },
        );
        Ok(())
    }

    /// The attempt [`Self::begin`] started failed: wait longer.
    pub fn failed(&self, name: &str, now: Instant) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.entry(key(name)).or_insert(Entry {
            failures: 0,
            next_allowed: now,
        });
        entry.failures = entry.failures.saturating_add(1);
        entry.next_allowed = now + delay(entry.failures);
    }

    /// The attempt succeeded: forget the name's failures.
    pub fn succeeded(&self, name: &str) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.remove(&key(name));
    }
}

/// Drop the forgotten entries, or failing that the one whose wait ended
/// longest ago.
fn make_room(entries: &mut HashMap<String, Entry>, now: Instant) {
    entries.retain(|_, entry| now < entry.next_allowed + FORGET_AFTER);
    if entries.len() < MAX_ENTRIES {
        return;
    }
    let oldest = entries
        .iter()
        .min_by_key(|(_, entry)| entry.next_allowed)
        .map(|(key, _)| key.clone());
    if let Some(oldest) = oldest {
        entries.remove(&oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn the_delay_doubles_up_to_the_cap() {
        let waits: Vec<_> = (1..=10).map(delay).collect();
        assert_eq!(
            waits,
            [1, 2, 4, 8, 16, 32, 64, 128, 256, 300].map(Duration::from_secs)
        );
        assert_eq!(delay(u32::MAX), MAX_DELAY);
    }

    #[test]
    fn failures_space_out_attempts_and_success_clears_them() {
        let throttle = LoginThrottle::default();
        let t0 = Instant::now();
        assert!(throttle.begin("erik", t0).is_ok());
        throttle.failed("erik", t0);
        assert_eq!(throttle.begin("erik", t0), Err(SECOND));
        // Another name is not affected.
        assert!(throttle.begin("anna", t0).is_ok());

        let t1 = t0 + SECOND;
        assert!(throttle.begin("erik", t1).is_ok());
        throttle.failed("erik", t1);
        assert_eq!(throttle.begin("erik", t1 + SECOND), Err(SECOND));

        let t2 = t1 + 2 * SECOND;
        assert!(throttle.begin("erik", t2).is_ok());
        throttle.succeeded("erik");
        assert!(throttle.begin("erik", t2).is_ok());
    }

    #[test]
    fn attempts_sent_together_are_spaced_like_attempts_in_turn() {
        let throttle = LoginThrottle::default();
        let t0 = Instant::now();
        assert!(throttle.begin("erik", t0).is_ok());
        // The first has not been answered yet.
        assert_eq!(throttle.begin("erik", t0), Err(SECOND));
    }

    #[test]
    fn an_old_failure_is_forgotten() {
        let throttle = LoginThrottle::default();
        let t0 = Instant::now();
        for i in 0..5 {
            let t = t0 + i * MAX_DELAY;
            assert!(throttle.begin("erik", t).is_ok());
            throttle.failed("erik", t);
        }
        let later = t0 + 5 * MAX_DELAY + FORGET_AFTER;
        assert!(throttle.begin("erik", later).is_ok());
        throttle.failed("erik", later);
        assert_eq!(throttle.begin("erik", later), Err(SECOND));
    }

    #[test]
    fn the_map_stays_bounded() {
        let throttle = LoginThrottle::default();
        let t0 = Instant::now();
        for i in 0..MAX_ENTRIES + 10 {
            let name = format!("made-up-{i}");
            let t = t0 + Duration::from_millis(i as u64);
            assert!(throttle.begin(&name, t).is_ok());
            throttle.failed(&name, t);
        }
        assert_eq!(throttle.entries.lock().unwrap().len(), MAX_ENTRIES);
        // The newest is still held back.
        let newest = format!("made-up-{}", MAX_ENTRIES + 9);
        assert!(throttle.begin(&newest, t0).is_err());
    }
}
