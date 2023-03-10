// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::HashSet,
    fmt,
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

pub type Error = notify::Error;

pub trait BatchHandler {
    fn handle_event(&mut self, result: Result<BatchEvent, Vec<Error>>);
}

impl<F: FnMut(Result<BatchEvent, Vec<Error>>)> BatchHandler for F {
    fn handle_event(&mut self, result: Result<BatchEvent, Vec<Error>>) {
        (self)(result)
    }
}

pub struct BatchWatcherBuilder<W: notify::Watcher> {
    config: notify::Config,
    event_filter: EventFilter,
    _phantom: PhantomData<W>,
}

impl<W> BatchWatcherBuilder<W>
where
    W: notify::Watcher,
{
    pub fn new() -> Self {
        Self {
            config: notify::Config::default(),
            event_filter: EventFilter(|_event| true),
            _phantom: PhantomData,
        }
    }

    pub fn notify_config(mut self, config: notify::Config) -> Self {
        self.config = config;
        self
    }

    /// Filter events. The batch watcher will only include events for which the closure returns
    /// true.
    pub fn event_filter(mut self, event_filter: fn(&notify::Event) -> bool) -> Self {
        self.event_filter = EventFilter(event_filter);
        self
    }

    pub fn build<H>(self, batch_timeout: Duration, mut handler: H) -> Result<BatchWatcher<W>, Error>
    where
        H: BatchHandler + Send + 'static,
    {
        let Self { event_filter, config, _phantom: _ } = self;

        let state = Arc::new(State::new(batch_timeout, event_filter));

        let watcher = {
            let state = Arc::clone(&state);
            W::new(
                move |result: notify::Result<notify::Event>| state.add_notify_event(result),
                config,
            )?
        };

        // FIXME(https://github.com/notify-rs/notify/issues/463): Create the batches in a background
        // thread. We might be able to migrate this over to an async task once this bug is fixed.
        //
        // This thread should automatically shut down once the channel is dropped, which happens
        // when the `notify::Watcher` is dropped.
        let thread = {
            let state = Arc::clone(&state);
            std::thread::Builder::new().name("notify batch watcher worker".into()).spawn(
                move || {
                    while let Some(event_data) = state.next_event_data() {
                        if !event_data.paths.is_empty() {
                            handler.handle_event(Ok(BatchEvent {
                                rescan: event_data.rescan,
                                start_time: event_data.start_time,
                                end_time: event_data.end_time,
                                paths: event_data.paths,
                            }));
                        }

                        if !event_data.errors.is_empty() {
                            handler.handle_event(Err(event_data.errors));
                        }
                    }
                },
            )?
        };

        Ok(BatchWatcher { watcher, state, thread: Some(thread) })
    }
}

/// Notify watchers can emit multiple events for the same file system operation. To avoid excess
/// churn, the `BatchWatcher` will batch up multiple `notify::Event` events into a single
/// `BatchEvent` event.
#[derive(Debug)]
pub struct BatchWatcher<W: notify::Watcher = notify::RecommendedWatcher> {
    watcher: W,
    state: Arc<State>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl<W: notify::Watcher> BatchWatcher<W> {
    pub fn builder() -> BatchWatcherBuilder<W> {
        BatchWatcherBuilder::new()
    }

    /// Watch a path for changes.
    pub fn watch(
        &mut self,
        path: &Path,
        recursive_mode: notify::RecursiveMode,
    ) -> Result<(), Error> {
        self.watcher.watch(path, recursive_mode)
    }

    /// Unwatch a path for changes.
    pub fn unwatch(&mut self, path: &Path) -> Result<(), Error> {
        self.watcher.unwatch(path)
    }

    /// Stop watching for changes. This will block until the background thread is torn down. This
    /// can take as long as `batch_timeout` seconds.
    ///
    /// If you do not want to wait for the thread to shut down, you can use
    /// `std::mem::drop` to have the thread exit in the background.
    pub fn stop(mut self) {
        self.state.set_stop();

        // Wait for the thread to shut down.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl<W: notify::Watcher> Drop for BatchWatcher<W> {
    fn drop(&mut self) {
        // Shut down the background thread in the background.
        self.state.set_stop();
    }
}

impl BatchWatcher<notify::RecommendedWatcher> {
    pub fn new<H>(batch_timeout: Duration, handler: H) -> Result<Self, Error>
    where
        H: BatchHandler + Send + 'static,
    {
        BatchWatcherBuilder::new().build(batch_timeout, handler)
    }
}

/// File system events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchEvent {
    /// If true, file system changes were observed, but some events were dropped. Clients may need
    /// to recompute internal state.
    pub rescan: bool,

    /// When the first batched event occurred.
    ///
    /// This is a measurement from a monotonically nondecreasing clock. It is guaranteed to be:
    ///
    /// * less than or equal to `Instant::now()`.
    /// * less than or equal to the `end_time` in this `BatchEvent`.
    /// * less than or equal to the `start_time` and `end_time` in the next `BatchEvent` that was
    ///   produced by the `BatchWatcher` that produced this `BatchEvent`.
    pub start_time: Instant,

    /// When the last batched event occurred.
    ///
    /// This is a measurement from a monotonically nondecreasing clock. It is guaranteed to be:
    ///
    /// * less than or equal to `Instant::now()`.
    /// * greater than or equal to the `start_time` in this `BatchEvent`.
    /// * less than or equal to the `start_time` and `end_time` in the next `BatchEvent` that was
    ///   produced by the `BatchWatcher` that produced this `BatchEvent`.
    pub end_time: Instant,

    /// Which paths were changed.
    pub paths: HashSet<PathBuf>,
}

#[derive(Debug)]
struct State {
    batch_timeout: Duration,
    event_filter: EventFilter,
    inner: Mutex<StateInner>,
    condvar: Condvar,
}

impl State {
    fn new(batch_timeout: Duration, event_filter: EventFilter) -> Self {
        Self {
            batch_timeout,
            event_filter,
            inner: Mutex::new(StateInner::Empty),
            condvar: Condvar::new(),
        }
    }

    fn set_stop(&self) {
        let mut inner = self.inner.lock().unwrap();
        *inner = StateInner::Stop;

        // Wake up the worker thread so it can shut down.
        self.condvar.notify_one();
    }

    fn add_notify_event(&self, result: notify::Result<notify::Event>) {
        let result = match result {
            Ok(event) => {
                // Check if we're interested in this event. Otherwise, if this event is a rescan,
                // filter out the paths, otherwise we can exit early.
                if (self.event_filter.0)(&event) {
                    Ok((event.need_rescan(), event.paths))
                } else if event.need_rescan() {
                    Ok((true, vec![]))
                } else {
                    return;
                }
            }
            Err(err) => Err(err),
        };

        // Merge the event into our data.
        let mut inner = self.inner.lock().unwrap();

        match &mut *inner {
            StateInner::Empty => {
                *inner = StateInner::Event(EventData::new(result));

                // Notify the thread we've started creating a new batch.
                self.condvar.notify_one();
            }
            StateInner::Event(data) => {
                data.merge(result);
            }
            StateInner::Stop => {}
        }
    }

    // Wait for the next batch event to be ready, or returns `None` if we were signaled to shut
    // down.
    fn next_event_data(&self) -> Option<EventData> {
        let mut inner = self.inner.lock().unwrap();

        let event_data = loop {
            match &*inner {
                StateInner::Empty => {
                    // Wait until we're signaled more data is available.
                    inner = self.condvar.wait(inner).unwrap();
                }
                StateInner::Event(event_data) => {
                    break event_data;
                }
                StateInner::Stop => {
                    return None;
                }
            }
        };

        // Check if we've passed the batch deadline.
        let deadline = event_data.start_time + self.batch_timeout;
        let Some(sleep_time) = deadline.checked_duration_since(Instant::now()) else {
            // Return the data since we've passed the deadline.
            if let StateInner::Event(event_data) = std::mem::replace(&mut *inner, StateInner::Empty) {
                return Some(event_data);
            } else {
                unreachable!()
            }
        };

        // Otherwise we need to wait for the batch deadline for it to be ready. Rather than directly
        // putting the thread to sleep, we'll let the condition variable wake us up if we're stopped
        // while sleeping. This will let us tear down the thread faster during shutdown.
        let (mut inner, result) = self
            .condvar
            .wait_timeout_while(inner, sleep_time, |inner| !matches!(inner, StateInner::Stop))
            .unwrap();

        // If we didn't time out, then that means we transitioned into the stop state.
        if !result.timed_out() {
            return None;
        }

        // Return the event data we accumulated.
        if let StateInner::Event(event_data) = std::mem::replace(&mut *inner, StateInner::Empty) {
            Some(event_data)
        } else {
            unreachable!()
        }
    }
}

#[derive(Debug)]
enum StateInner {
    Empty,
    Event(EventData),
    Stop,
}

#[derive(Debug)]
struct EventData {
    start_time: Instant,
    end_time: Instant,
    rescan: bool,
    paths: HashSet<PathBuf>,
    errors: Vec<notify::Error>,
}

impl EventData {
    fn new(result: notify::Result<(bool, Vec<PathBuf>)>) -> Self {
        let (rescan, paths, errors) = match result {
            Ok((rescan, paths)) => (rescan, paths.into_iter().collect(), vec![]),
            Err(err) => (false, HashSet::new(), vec![err]),
        };

        let now = Instant::now();
        EventData { start_time: now, end_time: now, rescan, paths, errors }
    }

    fn merge(&mut self, result: notify::Result<(bool, Vec<PathBuf>)>) {
        self.end_time = Instant::now();

        match result {
            Ok((rescan, paths)) => {
                self.rescan |= rescan;
                self.paths.extend(paths);
            }
            Err(err) => {
                self.errors.push(err);
            }
        }
    }
}

struct EventFilter(fn(&notify::Event) -> bool);

impl fmt::Debug for EventFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("EventFilter").field(&"..").finish()
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        pretty_assertions::assert_eq,
        std::{fs::File, io::Write, path::Path, sync::mpsc},
    };

    const BATCH_DURATION: Duration = Duration::from_millis(100);

    fn create_file(file_path: &Path, bytes: &[u8]) {
        let mut file = File::create(file_path).unwrap();
        file.write_all(bytes).unwrap();
    }

    fn update_file(path: &Path, bytes: &[u8]) {
        let mut file = std::fs::OpenOptions::new().write(true).truncate(true).open(path).unwrap();
        file.write_all(bytes).unwrap();
    }

    #[track_caller]
    fn check_event_paths(event: &BatchEvent, expected_paths: impl IntoIterator<Item = PathBuf>) {
        assert_eq!(event.paths, expected_paths.into_iter().collect::<HashSet<_>>());
    }

    fn create_watcher(
        dir: &Path,
    ) -> (BatchWatcher, mpsc::Receiver<Result<BatchEvent, Vec<Error>>>) {
        let (event_tx, event_rx) = mpsc::sync_channel(1);

        let mut watcher = BatchWatcher::new(BATCH_DURATION, move |event| {
            event_tx.send(event).unwrap();
        })
        .unwrap();

        watcher.watch(dir, notify::RecursiveMode::Recursive).unwrap();

        // Some notify backends can observe file system events that occurred before the watcher
        // is set up (for example, with FSEvents and OSX). To avoid this from confusing the
        // tests, ignore if any files are observed.
        match event_rx.recv_timeout(BATCH_DURATION * 10) {
            Ok(Ok(event)) => {
                check_event_paths(&event, [dir.into()]);
            }
            Ok(Err(err)) => {
                panic!("unexpected error {err:?}");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("unexpected disconnection");
            }
        }

        (watcher, event_rx)
    }

    #[test]
    fn test_create_then_modify_then_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let tmpdir = tmp.path().canonicalize().unwrap();

        let (watcher, event_rx) = create_watcher(&tmpdir);

        let file_path = tmpdir.join("file1");
        create_file(&file_path, b"hello1");
        update_file(&file_path, b"there1");
        std::fs::remove_file(&file_path).unwrap();

        std::thread::sleep(BATCH_DURATION * 10);
        let event1 = event_rx.recv().unwrap().unwrap();
        check_event_paths(&event1, [file_path]);

        assert!(event1.start_time <= event1.end_time);
        assert!(event1.end_time <= Instant::now());

        let file_path = tmpdir.join("file2");
        create_file(&file_path, b"hello2");
        update_file(&file_path, b"there2");
        std::fs::remove_file(&file_path).unwrap();

        std::thread::sleep(BATCH_DURATION * 10);
        let event2 = event_rx.recv().unwrap().unwrap();
        check_event_paths(&event2, [file_path]);

        assert!(event2.start_time <= event2.end_time);
        assert!(event1.end_time <= event2.start_time);
        assert!(event2.end_time <= Instant::now());

        watcher.stop();
    }
}
