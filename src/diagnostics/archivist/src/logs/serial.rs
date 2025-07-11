// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::logs::repository::LogsRepository;
use crate::logs::servers::LogFreezeServer;
use anyhow::Error;
use diagnostics_data::{Data, Logs};
use fidl_fuchsia_diagnostics::{Selector, StreamMode};
use fidl_fuchsia_diagnostics_system::SerialLogControlRequestStream;
use fuchsia_async::{yield_now, OnSignals};
use fuchsia_trace as ftrace;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures::future::{select, Either};
use futures::{FutureExt, StreamExt};
use log::warn;
use selectors::FastError;
use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::Display;
use std::io::{self, Write};
use std::pin::pin;
use std::sync::Arc;
use zx::Signals;

const MAX_SERIAL_WRITE_SIZE: usize = 256;

/// Function that forwards logs from Archivist
/// to the serial port. Logs will be filtered by allow_serial_log_tags
/// to include logs in the serial output, and deny_serial_log_tags to exclude specific tags.
pub async fn launch_serial(
    allow_serial_log_tags: Vec<String>,
    deny_serial_log_tags: Vec<String>,
    logs_repo: Arc<LogsRepository>,
    writer: impl Write,
    mut freeze_receiver: UnboundedReceiver<SerialLogControlRequestStream>,
    mut flush_receiver: UnboundedReceiver<UnboundedSender<()>>,
) {
    let mut write_logs_to_serial =
        pin!(SerialConfig::new(allow_serial_log_tags, deny_serial_log_tags)
            .write_logs(logs_repo, writer)
            .fuse());
    let mut poll_flush = pin!(async {
        loop {
            let Some(flush_request) = flush_receiver.next().await else {
                break;
            };
            // Yield to the executor to allow for logs to be polled.
            // Because we're using select, the polling order is:
            // write_logs_to_serial, (our future).
            // When we yield, we should first write_logs_to_serial,
            // then poll this future again, so we know we've flushed
            // after we return from the yield operation.
            yield_now().await;
            // The caller of Flush may have dropped its channel, so it's OK
            // to ignore the result here.
            let _ = flush_request.unbounded_send(());
        }
    });
    loop {
        let log_freezer_future = pin!(async {
            // Wait for FDIO to give us the channel
            let stream = (freeze_receiver.next().await)?;
            let (client, server) = zx::EventPair::create();
            // Acquire the lock, and send the token.
            LogFreezeServer::new(client).wait_for_client_freeze_request(stream).await;
            Some(server)
        }
        .fuse());
        let maybe_frozen_token =
            select(select(&mut write_logs_to_serial, &mut poll_flush), log_freezer_future).await;
        if let Either::Right((Some(token), _)) = maybe_frozen_token {
            // Lock acquired, wait for it to be released before doing anything else.
            // Ignore any errors, as we may either get PEER_CLOSED as an error or signal.
            let _ = OnSignals::new(&token, Signals::EVENTPAIR_PEER_CLOSED).await;
        } else {
            // Serial writer or flush server exited, no work left to do
            // (Archivist is shutting down).
            break;
        }
    }
}

#[derive(Default)]
pub struct SerialConfig {
    selectors: Vec<Selector>,
    denied_tags: HashSet<String>,
}

impl SerialConfig {
    /// Creates a new serial configuration from the given structured config values.
    pub fn new<C, T>(allowed_components: Vec<C>, denied_tags: Vec<T>) -> Self
    where
        C: AsRef<str> + Display,
        T: Into<String>,
    {
        let selectors = allowed_components
            .into_iter()
            .filter_map(|selector| {
                match selectors::parse_component_selector::<FastError>(selector.as_ref()) {
                    Ok(s) => Some(Selector {
                        component_selector: Some(s),
                        tree_selector: None,
                        ..Selector::default()
                    }),
                    Err(err) => {
                        warn!(selector:%, err:?; "Failed to parse component selector");
                        None
                    }
                }
            })
            .collect();
        Self { selectors, denied_tags: HashSet::from_iter(denied_tags.into_iter().map(Into::into)) }
    }

    /// Returns a future that resolves when there's no more logs to write to serial. This can only
    /// happen when all log sink connections have been closed for the components that were
    /// configured to emit logs.
    pub async fn write_logs<S: Write>(self, repo: Arc<LogsRepository>, mut sink: S) {
        let Self { denied_tags, selectors } = self;
        let mut log_stream = repo.logs_cursor(
            StreamMode::SnapshotThenSubscribe,
            Some(selectors),
            ftrace::Id::random(),
        );
        while let Some(log) = log_stream.next().await {
            SerialWriter::log(log.as_ref(), &denied_tags, &mut sink).ok();
        }
    }
}

/// A sink to write to serial. This Write implementation must be used together with SerialWriter.
#[derive(Default)]
pub struct SerialSink;

impl Write for SerialSink {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if cfg!(debug_assertions) {
            debug_assert!(buffer.len() <= MAX_SERIAL_WRITE_SIZE);
        } else {
            use std::sync::atomic::{AtomicBool, Ordering};
            static ALREADY_LOGGED: AtomicBool = AtomicBool::new(false);
            if buffer.len() > MAX_SERIAL_WRITE_SIZE && !ALREADY_LOGGED.swap(true, Ordering::Relaxed)
            {
                let size = buffer.len();
                log::error!(
                    size;
                    "Skipping write to serial due to internal error. Exceeded max buffer size."
                );
                return Ok(buffer.len());
            }
        }
        // SAFETY: calling a syscall. We pass a pointer to the buffer and its exact size.
        unsafe {
            zx::sys::zx_debug_write(buffer.as_ptr(), buffer.len());
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct SerialWriter<'a, S> {
    buffer: Vec<u8>,
    denied_tags: &'a HashSet<String>,
    sink: &'a mut S,
}

impl<S: Write> Write for SerialWriter<'_, S> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        // -1 since we always write a `\n` when flushing.
        let count = (self.buffer.capacity() - self.buffer.len() - 1).min(data.len());
        let actual_count = self.buffer.write(&data[..count])?;
        debug_assert_eq!(actual_count, count);
        if self.buffer.len() == self.buffer.capacity() - 1 {
            self.flush()?;
        }
        Ok(actual_count)
    }

    fn flush(&mut self) -> io::Result<()> {
        debug_assert!(self.buffer.len() < MAX_SERIAL_WRITE_SIZE);
        let wrote = self.buffer.write(b"\n")?;
        debug_assert_eq!(wrote, 1);
        self.sink.write_all(self.buffer.as_slice())?;
        self.buffer.clear();
        Ok(())
    }
}

impl<'a, S: Write> SerialWriter<'a, S> {
    fn log(
        log: &Data<Logs>,
        denied_tags: &'a HashSet<String>,
        sink: &'a mut S,
    ) -> Result<(), Error> {
        let mut this =
            Self { buffer: Vec::with_capacity(MAX_SERIAL_WRITE_SIZE), sink, denied_tags };
        write!(
            &mut this,
            "[{:05}.{:03}] {:05}:{:05}> [",
            zx::MonotonicDuration::from_nanos(log.metadata.timestamp.into_nanos()).into_seconds(),
            zx::MonotonicDuration::from_nanos(log.metadata.timestamp.into_nanos()).into_millis()
                % 1000,
            log.pid().unwrap_or(0),
            log.tid().unwrap_or(0)
        )?;

        let empty_tags = log.tags().map(|tags| tags.is_empty()).unwrap_or(true);
        if empty_tags {
            write!(&mut this, "{}", log.component_name())?;
        } else {
            // Unwrap is safe, if we are here it means that we actually have tags.
            let tags = log.tags().unwrap();
            for (i, tag) in tags.iter().enumerate() {
                if this.denied_tags.contains(tag) {
                    return Ok(());
                }
                write!(&mut this, "{tag}")?;
                if i < tags.len() - 1 {
                    write!(&mut this, ", ")?;
                }
            }
        }

        write!(&mut this, "] {}: ", log.severity())?;
        let mut pending_message_parts = [Cow::Borrowed(log.msg().unwrap_or(""))]
            .into_iter()
            .chain(log.payload_keys_strings().map(|s| Cow::Owned(format!(" {s}"))));
        let mut pending_str = None;

        loop {
            let (data, offset) = match pending_str.take() {
                Some((s, offset)) => (s, offset),
                None => match pending_message_parts.next() {
                    Some(s) => (s, 0),
                    None => break,
                },
            };
            let count = this.write(&data.as_bytes()[offset..])?;
            if offset + count < data.len() {
                pending_str = Some((data, offset + count));
            }
        }
        if !this.buffer.is_empty() {
            this.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_diagnostics_system::SerialLogControlMarker;
    use fuchsia_async::{self as fasync};
    use futures::channel::mpsc::{self, unbounded};
    use futures::SinkExt;
    use std::future::{poll_fn, Future};
    use std::task::Poll;

    use super::*;
    use crate::identity::ComponentIdentity;
    use crate::logs::testing::make_message;
    use diagnostics_data::{BuilderArgs, LogsDataBuilder, LogsField, LogsProperty, Severity};
    use futures::FutureExt;
    use moniker::ExtendedMoniker;
    use std::pin::pin;
    use zx::BootInstant;

    struct TestSink {
        snd: mpsc::UnboundedSender<String>,
    }

    impl TestSink {
        fn new() -> (Self, mpsc::UnboundedReceiver<String>) {
            let (snd, rcv) = mpsc::unbounded();
            (Self { snd }, rcv)
        }
    }

    impl Write for TestSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let string = String::from_utf8(buf.to_vec()).expect("wrote valid utf8");
            self.snd.unbounded_send(string).expect("sent item");
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[fuchsia::test]
    fn write_to_serial_handles_denied_tags() {
        let log = LogsDataBuilder::new(BuilderArgs {
            timestamp: BootInstant::from_nanos(1),
            component_url: Some("url".into()),
            moniker: "core/foo".try_into().unwrap(),
            severity: Severity::Info,
        })
        .add_tag("denied-tag")
        .build();
        let denied_tags = HashSet::from_iter(["denied-tag".to_string()]);
        let mut sink = Vec::new();
        SerialWriter::log(&log, &denied_tags, &mut sink).expect("write succeeded");
        assert!(sink.is_empty());
    }

    #[fuchsia::test]
    fn write_to_serial_splits_lines() {
        let message = concat!(
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Aliquam accumsan eu neque ",
            "quis molestie. Nam rhoncus sapien non eleifend tristique. Duis quis turpis volutpat ",
            "neque bibendum molestie. Etiam ac sapien justo. Nullam aliquet ipsum nec tincidunt."
        );
        let log = LogsDataBuilder::new(BuilderArgs {
            timestamp: BootInstant::from_nanos(123456789),
            component_url: Some("url".into()),
            moniker: "core/foo".try_into().unwrap(),
            severity: Severity::Info,
        })
        .add_tag("bar")
        .set_message(message)
        .add_key(LogsProperty::String(LogsField::Other("key".to_string()), "value".to_string()))
        .add_key(LogsProperty::Int(LogsField::Other("other_key".to_string()), 3))
        .set_pid(1234)
        .set_tid(5678)
        .build();
        let mut sink = Vec::new();
        SerialWriter::log(&log, &HashSet::new(), &mut sink).expect("write succeeded");
        assert_eq!(
            String::from_utf8(sink).unwrap(),
            format!(
                "[00000.123] 01234:05678> [bar] INFO: {}\n{} key=value other_key=3\n",
                &message[..218],
                &message[218..]
            )
        );
    }

    #[fuchsia::test]
    fn when_no_tags_are_present_the_component_name_is_used() {
        let log = LogsDataBuilder::new(BuilderArgs {
            timestamp: BootInstant::from_nanos(123456789),
            component_url: Some("url".into()),
            moniker: "core/foo".try_into().unwrap(),
            severity: Severity::Info,
        })
        .set_message("my msg")
        .set_pid(1234)
        .set_tid(5678)
        .build();
        let mut sink = Vec::new();
        SerialWriter::log(&log, &HashSet::new(), &mut sink).expect("write succeeded");
        assert_eq!(
            String::from_utf8(sink).unwrap(),
            "[00000.123] 01234:05678> [foo] INFO: my msg\n"
        );
    }

    async fn poll_once<F: Future + Unpin>(mut future: F) {
        poll_fn(|context| {
            let _ = future.poll_unpin(context);
            Poll::Ready(())
        })
        .await;
    }

    #[fuchsia::test]
    async fn pauses_logs_correctly() {
        let repo = LogsRepository::for_test(fasync::Scope::new());

        let bootstrap_foo_container = repo.get_log_container(Arc::new(ComponentIdentity::new(
            ExtendedMoniker::parse_str("./bootstrap/foo").unwrap(),
            "fuchsia-pkg://bootstrap-foo",
        )));
        let bootstrap_bar_container = repo.get_log_container(Arc::new(ComponentIdentity::new(
            ExtendedMoniker::parse_str("./bootstrap/bar").unwrap(),
            "fuchsia-pkg://bootstrap-bar",
        )));

        let core_foo_container = repo.get_log_container(Arc::new(ComponentIdentity::new(
            ExtendedMoniker::parse_str("./core/foo").unwrap(),
            "fuchsia-pkg://core-foo",
        )));
        let core_baz_container = repo.get_log_container(Arc::new(ComponentIdentity::new(
            ExtendedMoniker::parse_str("./core/baz").unwrap(),
            "fuchsia-pkg://core-baz",
        )));

        bootstrap_foo_container.ingest_message(make_message(
            "a",
            None,
            zx::BootInstant::from_nanos(1),
        ));

        core_baz_container.ingest_message(make_message("c", None, zx::BootInstant::from_nanos(2)));
        let (sink, mut rcv) = TestSink::new();
        let cloned_repo = Arc::clone(&repo);
        let (mut sender, receiver) = unbounded();
        let (_flush_sender, flush_receiver) = unbounded();
        let mut serial_task = pin!(async move {
            let allowed = vec!["bootstrap/**".into(), "/core/foo".into()];
            let denied = vec!["foo".into()];
            launch_serial(allowed, denied, cloned_repo, sink, receiver, flush_receiver).await;
        }
        .fuse());
        bootstrap_bar_container.ingest_message(make_message(
            "b",
            Some("foo"),
            zx::BootInstant::from_nanos(3),
        ));

        poll_once(&mut serial_task).await;
        let received = rcv.next().now_or_never().unwrap().unwrap();

        assert_eq!(received, "[00000.000] 00001:00002> [foo] DEBUG: a\n");

        let (client, server) = create_proxy_and_stream::<SerialLogControlMarker>();
        sender.send(server).await.unwrap();
        let freeze_token = futures::select! {
            _ = serial_task => None,
            token = client.freeze_serial_forwarding().fuse() => Some(token),
        }
        .unwrap();
        core_foo_container.ingest_message(make_message("c", None, zx::BootInstant::from_nanos(4)));
        let received_future = rcv.next();
        poll_once(&mut serial_task).await;

        assert!(received_future.now_or_never().is_none());
        drop(freeze_token);
        poll_once(&mut serial_task).await;
        let received = rcv.next().now_or_never().unwrap().unwrap();

        assert_eq!(received, "[00000.000] 00001:00002> [foo] DEBUG: c\n");
    }

    #[fuchsia::test]
    async fn writes_ingested_logs() {
        let serial_config = SerialConfig::new(vec!["bootstrap/**", "/core/foo"], vec!["foo"]);
        let repo = LogsRepository::for_test(fasync::Scope::new());

        let bootstrap_foo_container = repo.get_log_container(Arc::new(ComponentIdentity::new(
            ExtendedMoniker::parse_str("./bootstrap/foo").unwrap(),
            "fuchsia-pkg://bootstrap-foo",
        )));
        let bootstrap_bar_container = repo.get_log_container(Arc::new(ComponentIdentity::new(
            ExtendedMoniker::parse_str("./bootstrap/bar").unwrap(),
            "fuchsia-pkg://bootstrap-bar",
        )));

        let core_foo_container = repo.get_log_container(Arc::new(ComponentIdentity::new(
            ExtendedMoniker::parse_str("./core/foo").unwrap(),
            "fuchsia-pkg://core-foo",
        )));
        let core_baz_container = repo.get_log_container(Arc::new(ComponentIdentity::new(
            ExtendedMoniker::parse_str("./core/baz").unwrap(),
            "fuchsia-pkg://core-baz",
        )));

        bootstrap_foo_container.ingest_message(make_message(
            "a",
            None,
            zx::BootInstant::from_nanos(1),
        ));
        core_baz_container.ingest_message(make_message("c", None, zx::BootInstant::from_nanos(2)));
        let (sink, rcv) = TestSink::new();
        let mut serial_task = pin!(serial_config.write_logs(Arc::clone(&repo), sink));
        bootstrap_bar_container.ingest_message(make_message(
            "b",
            Some("foo"),
            zx::BootInstant::from_nanos(3),
        ));
        core_foo_container.ingest_message(make_message("c", None, zx::BootInstant::from_nanos(4)));
        poll_fn(|context| loop {
            if Poll::Pending == serial_task.poll_unpin(context) {
                return Poll::Ready(());
            }
        })
        .await;
        let received = rcv.take(2).collect::<Vec<_>>().now_or_never().unwrap();

        // We must see the logs emitted before we installed the serial listener and after. We must
        // not see the log from /core/baz and we must not see the log from bootstrap/bar with tag
        // "foo".
        assert_eq!(
            received,
            vec![
                "[00000.000] 00001:00002> [foo] DEBUG: a\n",
                "[00000.000] 00001:00002> [foo] DEBUG: c\n"
            ]
        );
    }
}
