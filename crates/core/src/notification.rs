//! Wake-up hints for a UI or another event-loop host.
use std::{fmt, sync::Arc, time::Duration};

/// Notifies a host when progress changes or a timed refresh is useful.
#[derive(Clone)]
pub struct Wake(Arc<dyn Fn(Duration) + Send + Sync>);
impl Wake {
    #[must_use]
    pub fn new(callback: impl Fn(Duration) + Send + Sync + 'static) -> Self {
        Self(Arc::new(callback))
    }
    pub fn notify(&self) {
        (self.0)(Duration::ZERO);
    }
    pub fn notify_after(&self, delay: Duration) {
        (self.0)(delay);
    }
}
impl Default for Wake {
    /// A headless caller may poll without an event-loop notification.
    fn default() -> Self {
        Self::new(|_| {})
    }
}
impl fmt::Debug for Wake {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Wake").finish_non_exhaustive()
    }
}
