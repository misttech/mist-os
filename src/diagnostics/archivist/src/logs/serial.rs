// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::logs::repository::LogsRepository;
use anyhow::Error;
use diagnostics_data::{Data, Logs};
use fidl_fuchsia_diagnostics::{Selector, StreamMode};
use fuchsia_trace as ftrace;
use futures::StreamExt;
use log::warn;
use selectors::FastError;
use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::Display;
use std::io::{self, Write};
use std::sync::Arc;

const MAX_SERIAL_WRITE_SIZE: usize = 256;

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
                write!(&mut this, "{}", tag)?;
                if i < tags.len() - 1 {
                    write!(&mut this, ", ")?;
                }
            }
        }

        write!(&mut this, "] {}: ", log.severity())?;
        let mut pending_message_parts = [Cow::Borrowed(log.msg().unwrap_or(""))]
            .into_iter()
            .chain(log.payload_keys_strings().map(|s| Cow::Owned(format!(" {}", s))));
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
    use super::*;
    use crate::identity::ComponentIdentity;
    use crate::logs::testing::make_message;
    use diagnostics_data::{BuilderArgs, LogsDataBuilder, LogsField, LogsProperty, Severity};
    use fuchsia_async as fasync;
    use futures::channel::mpsc;
    use moniker::ExtendedMoniker;
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
        let _serial_task = fasync::Task::spawn(serial_config.write_logs(Arc::clone(&repo), sink));
        bootstrap_bar_container.ingest_message(make_message(
            "b",
            Some("foo"),
            zx::BootInstant::from_nanos(3),
        ));
        core_foo_container.ingest_message(make_message("c", None, zx::BootInstant::from_nanos(4)));

        let received = rcv.take(2).collect::<Vec<_>>().await;

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
