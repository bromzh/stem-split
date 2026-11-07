//! Debounced, cancellable metadata requests. At most one worker owns a tool.

use std::{
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::Wake;
use crate::{CancelToken, model::Usage};
use stem_split_media_fetching::{Inspected, MediaInfo, MediaUrl};

use crate::worker::Failure;

const DEBOUNCE: Duration = Duration::from_millis(500);

/// Whether metadata inspection may start while the host handles other operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Active,
    Paused,
}

#[derive(Debug)]
pub enum State {
    Empty,
    Pending { url: MediaUrl, changed: Instant },
    Loading(MediaUrl),
    Ready(MediaInfo),
    Failed(Failure),
}

#[derive(Debug)]
struct Request {
    url: MediaUrl,
    cancel: CancelToken,
    result: mpsc::Receiver<(Result<Inspected, Failure>, Option<Usage>)>,
    thread: JoinHandle<()>,
}

#[derive(Debug)]
pub struct Preview {
    state: State,
    request: Option<Request>,
    usage: Option<Usage>,
}

impl Default for Preview {
    fn default() -> Self {
        Self {
            state: State::Empty,
            request: None,
            usage: None,
        }
    }
}

impl Preview {
    pub fn take_usage(&mut self) -> Option<Usage> {
        self.usage.take()
    }

    #[must_use]
    pub const fn is_busy(&self) -> bool {
        self.request.is_some()
    }

    #[must_use]
    pub const fn state(&self) -> &State {
        &self.state
    }

    pub fn change(&mut self, value: &str, now: Instant) {
        self.cancel();
        self.state =
            MediaUrl::new(value).map_or(State::Empty, |url| State::Pending { url, changed: now });
    }

    pub fn cancel(&mut self) {
        if let Some(request) = &self.request {
            request.cancel.cancel();
        }
        if let State::Loading(url) = &self.state {
            self.state = State::Pending {
                url: url.clone(),
                changed: Instant::now(),
            };
        }
    }

    pub fn poll(
        &mut self,
        context: &Wake,
        access: Access,
        now: Instant,
        cache: &crate::config::CacheLocation,
    ) {
        let enabled = access == Access::Active;
        if !enabled {
            self.cancel();
        }
        if self
            .request
            .as_ref()
            .is_some_and(|r| r.thread.is_finished())
            && let Some(request) = self.request.take()
        {
            let (result, usage) = match request.thread.join() {
                Ok(()) => request
                    .result
                    .try_recv()
                    .unwrap_or((Err(Failure::Crashed), None)),
                Err(_) => (Err(Failure::Crashed), None),
            };
            self.usage = usage.or(self.usage);
            self.receive(request.url, result, now);
        }
        if enabled
            && self.request.is_none()
            && let State::Pending { url, changed } = &self.state
        {
            let remaining = DEBOUNCE.saturating_sub(now.saturating_duration_since(*changed));
            if remaining.is_zero() {
                let url = url.clone();
                self.state = match Request::start(url.clone(), context.clone(), cache.clone()) {
                    Ok(request) => {
                        self.request = Some(request);
                        State::Loading(url)
                    }
                    Err(error) => State::Failed(Failure::NotStarted(error)),
                };
            } else {
                context.notify_after(remaining);
            }
        }
        if self.request.is_some() {
            context.notify_after(Duration::from_millis(100));
        }
    }
    fn receive(&mut self, url: MediaUrl, result: Result<Inspected, Failure>, now: Instant) {
        if matches!(&self.state, State::Loading(active) if *active == url) {
            self.state = match result {
                Ok(Inspected::Ready(info)) => State::Ready(info),
                Ok(Inspected::Cancelled) => State::Pending { url, changed: now },
                Err(error) => State::Failed(error),
            };
        }
    }

    pub fn stop(&mut self, patience: Duration) {
        self.cancel();
        let Some(request) = self.request.take() else {
            return;
        };
        let deadline = Instant::now() + patience;
        while !request.thread.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if request.thread.is_finished() && request.thread.join().is_err() {
            log::warn!("metadata worker panicked during shutdown");
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn show(&mut self, state: State) {
        self.state = state;
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Request {
    fn start(
        url: MediaUrl,
        context: Wake,
        cache: crate::config::CacheLocation,
    ) -> std::io::Result<Self> {
        let (sender, result) = mpsc::channel();
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let source = url.clone();
        let thread = thread::Builder::new()
            .name("URL metadata".to_owned())
            .spawn(move || {
                let result = match cache.store() {
                    Ok(store) => {
                        let result =
                            stem_split_media_fetching::inspect(&source, store.directory(), &token)
                                .map_err(Failure::Media);
                        let usage = match store.usage() {
                            Ok(usage) => Some(usage),
                            Err(error) => {
                                log::warn!("cannot measure media cache: {error}");
                                None
                            }
                        };
                        (result, usage)
                    }
                    Err(error) => (Err(Failure::Job(error.into())), None),
                };
                match sender.send(result) {
                    Ok(()) | Err(_) => {}
                }
                context.notify();
            })?;
        Ok(Self {
            url,
            cancel,
            result,
            thread,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_restart_the_delay_and_clear_stale_results()
    -> Result<(), stem_split_media_fetching::UrlError> {
        let mut preview = Preview::default();
        let now = Instant::now();
        let context = Wake::default();
        let original = MediaUrl::new("https://example.org/first")?;
        preview.change(original.as_str(), now);
        preview.poll(
            &context,
            Access::Active,
            now + Duration::from_millis(499),
            &crate::config::CacheLocation::UserCache,
        );
        assert!(preview.request.is_none());
        preview.change(
            "https://example.org/second",
            now + Duration::from_millis(499),
        );
        preview.poll(
            &context,
            Access::Active,
            now + Duration::from_millis(501),
            &crate::config::CacheLocation::UserCache,
        );
        assert!(preview.request.is_none());
        assert!(
            matches!(&preview.state, State::Pending { url, .. } if url.as_str().ends_with("second"))
        );
        preview.receive(original, Err(Failure::Crashed), now);
        assert!(matches!(preview.state, State::Pending { .. }));
        preview.change("", now);
        assert!(matches!(preview.state, State::Empty));
        Ok(())
    }
}
