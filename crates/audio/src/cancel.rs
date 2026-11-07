use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// A request to stop long-running work, shared between the thread that asks
/// and the thread that does the work.
///
/// Cancellation is cooperative: the worker checks the token between units of
/// work (a downloaded block, a separated chunk) and winds down by itself.
/// Clones share one flag, and a cancelled token stays cancelled.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_token_is_not_cancelled() {
        assert!(!CancelToken::new().is_cancelled());
        assert!(!CancelToken::default().is_cancelled());
    }

    #[test]
    fn clones_share_the_flag() {
        let token = CancelToken::new();
        let clone = token.clone();

        clone.cancel();

        assert!(token.is_cancelled());
        assert!(clone.is_cancelled());
    }

    #[test]
    fn cancelling_twice_is_harmless() {
        let token = CancelToken::new();
        token.cancel();
        token.cancel();
        assert!(token.is_cancelled());
    }
}
