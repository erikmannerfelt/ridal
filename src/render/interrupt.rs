//! Stopping a render part way through (#473).
//!
//! A render runs on a blocking thread, which cannot be cancelled from
//! outside, and holds a render permit and its radargram's lock until it
//! returns. A render that never returns (#472 made one take hours) holds
//! both for as long, so the renders that read the whole radargram check an
//! [`Interrupt`] between bands and give up when it fires: when the request
//! that wanted the image has gone away, or when the render has run past its
//! deadline.
//!
//! Checked between bands, so a render stops within one band of the
//! interrupt firing. A band is bounded by the read budget in
//! [`super::renderer`], which is what makes that prompt.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Why a render was stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interruption {
    /// Whoever asked for it no longer wants it.
    Cancelled,
    /// It ran past its deadline.
    TimedOut,
}

/// When a render should stop. The default never fires, which is what the
/// command line uses: someone running `ridal render` stops it themselves.
#[derive(Debug, Clone, Default)]
pub struct Interrupt {
    cancelled: Option<Arc<AtomicBool>>,
    deadline: Option<(Instant, Duration)>,
}

impl Interrupt {
    /// An interrupt that never fires.
    pub fn never() -> Self {
        Self::default()
    }

    /// An interrupt that fires when the returned guard is dropped.
    ///
    /// The guard belongs to whatever wants the result: kept in a request
    /// handler's future, it is dropped when the client goes away, which
    /// stops the render on the blocking thread at its next check.
    pub fn on_drop() -> (Self, CancelOnDrop) {
        let flag = Arc::new(AtomicBool::new(false));
        let interrupt = Self {
            cancelled: Some(Arc::clone(&flag)),
            deadline: None,
        };
        (interrupt, CancelOnDrop(flag))
    }

    /// This interrupt, firing `limit` from now as well.
    ///
    /// Set when the render starts, not when it was asked for: time spent
    /// waiting for a permit or a lock is bounded by those, and counting it
    /// here would time out the render that waited behind a slow one.
    pub fn with_deadline(self, limit: Duration) -> Self {
        Self {
            deadline: Some((Instant::now() + limit, limit)),
            ..self
        }
    }

    /// Whether, and why, the render should stop now.
    pub fn interruption(&self) -> Option<Interruption> {
        if self
            .cancelled
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Some(Interruption::Cancelled);
        }
        if self
            .deadline
            .is_some_and(|(deadline, _)| Instant::now() >= deadline)
        {
            return Some(Interruption::TimedOut);
        }
        None
    }

    /// How long the render was allowed, when it has a deadline.
    pub fn limit(&self) -> Option<Duration> {
        self.deadline.map(|(_, limit)| limit)
    }

    /// `Err` with a message naming why, once the interrupt has fired.
    ///
    /// The renderer's errors are strings, so a caller that needs to tell an
    /// interruption from a failure asks [`Self::interruption`] rather than
    /// reading this message.
    pub fn check(&self) -> Result<(), String> {
        match self.interruption() {
            None => Ok(()),
            Some(Interruption::Cancelled) => Err("The render was cancelled.".to_string()),
            Some(Interruption::TimedOut) => Err(format!(
                "The render took longer than the {} s it is allowed.",
                self.limit().map_or(0, |limit| limit.as_secs())
            )),
        }
    }
}

/// Fires its [`Interrupt`] when dropped.
#[derive(Debug)]
pub struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_fires() {
        let interrupt = Interrupt::never();
        assert_eq!(interrupt.interruption(), None);
        assert!(interrupt.check().is_ok());
    }

    #[test]
    fn dropping_the_guard_cancels() {
        let (interrupt, guard) = Interrupt::on_drop();
        let interrupt = interrupt.with_deadline(Duration::from_secs(3600));
        assert_eq!(interrupt.interruption(), None);
        drop(guard);
        assert_eq!(interrupt.interruption(), Some(Interruption::Cancelled));
        assert!(interrupt.check().is_err());
    }

    #[test]
    fn a_passed_deadline_times_out() {
        let interrupt = Interrupt::never().with_deadline(Duration::ZERO);
        assert_eq!(interrupt.interruption(), Some(Interruption::TimedOut));
        assert_eq!(interrupt.limit(), Some(Duration::ZERO));
    }
}
