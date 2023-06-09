// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{identity::ComponentIdentity, logs::stored_message::StoredMessage};
use anyhow::Error;
use diagnostics_data::{Data, Logs};
use fidl_fuchsia_diagnostics::ComponentSelector;
use fuchsia_zircon as zx;
use selectors::FastError;
use std::{
    borrow::Cow,
    collections::HashSet,
    fmt::Display,
    io::{self, BufWriter, Write},
    sync::Arc,
};
use tracing::{debug, warn};

const MAX_SERIAL_WRITE_SIZE: usize = 256;

#[derive(Default)]
pub struct SerialConfig {
    allowed_components: Vec<ComponentSelector>,
    denied_tags: Arc<HashSet<String>>,
}

impl SerialConfig {
    /// Creates a new serial configuration from the given structured config values.
    pub fn new<S, T>(allowed_components: Vec<S>, denied_tags: Vec<T>) -> Self
    where
        S: AsRef<str> + Display,
        T: Into<String>,
    {
        let selectors = allowed_components
            .into_iter()
            .filter_map(|selector| {
                match selectors::parse_component_selector::<FastError>(selector.as_ref()) {
                    Ok(s) => Some(s),
                    Err(err) => {
                        warn!(%selector, ?err, "Failed to parse component selector");
                        None
                    }
                }
            })
            .collect();
        Self {
            allowed_components: selectors,
            denied_tags: Arc::new(HashSet::from_iter(denied_tags.into_iter().map(Into::into))),
        }
    }

    /// Creates a serial configuration for a component or returns None if the component isn't
    /// allowed to log to serial.
    pub fn for_component(&self, identity: &ComponentIdentity) -> Option<ComponentSerialConfig> {
        if self.allowed_components.iter().any(|selector| {
            selectors::match_moniker_against_component_selector(
                identity.relative_moniker.iter(),
                selector,
            )
            .unwrap_or(false)
        }) {
            return Some(ComponentSerialConfig::new(Arc::clone(&self.denied_tags)));
        }
        None
    }
}

trait SerialResultSink {
    fn write_buffer(&mut self, buffer: &[u8]);
}

#[derive(Default)]
struct Serial;

impl SerialResultSink for Serial {
    fn write_buffer(&mut self, buffer: &[u8]) {
        // SAFETY: calling a syscall. We pass a pointer to the buffer and its exact size.
        unsafe {
            fuchsia_zircon::sys::zx_debug_write(buffer.as_ptr(), buffer.len());
        }
    }
}

struct SerialWriter<S> {
    buffer: Vec<u8>,
    sink: S,
}

impl<S: Default> SerialWriter<S> {
    fn new() -> Self {
        Self { buffer: Vec::with_capacity(MAX_SERIAL_WRITE_SIZE), sink: S::default() }
    }
}

impl<S> Write for SerialWriter<S>
where
    S: SerialResultSink,
{
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
        self.sink.write_buffer(self.buffer.as_slice());
        self.buffer.clear();
        Ok(())
    }
}

#[derive(Debug)]
pub struct ComponentSerialConfig {
    denied_tags: Arc<HashSet<String>>,
}

impl ComponentSerialConfig {
    fn new(denied_tags: Arc<HashSet<String>>) -> Self {
        Self { denied_tags }
    }

    /// Writes the log to serial if the log doesn't contain a tag that is in the set of denied log
    /// tags.
    pub fn maybe_write_to_serial(&self, log: &StoredMessage, identity: &ComponentIdentity) {
        let Ok(log) = log.parse(identity) else {
            return;
        };

        if let Err(err) = self.maybe_write_log(log) {
            debug!(?err, "Failed to write log to serial");
        }
    }

    fn maybe_write_log(&self, log: Data<Logs>) -> Result<(), Error> {
        let mut writer = SerialWriter::<Serial>::new();
        write_to_serial(log, &self.denied_tags, &mut writer)?;
        Ok(())
    }
}

fn write_to_serial<W: Write>(
    log: Data<Logs>,
    denied_tags: &HashSet<String>,
    writer: &mut W,
) -> Result<(), Error> {
    let mut writer = BufWriter::with_capacity(MAX_SERIAL_WRITE_SIZE, writer);
    write!(
        writer,
        "{:05}.{:03} {:05}:{:05} [",
        zx::Duration::from_nanos(log.metadata.timestamp).into_seconds(),
        zx::Duration::from_nanos(log.metadata.timestamp).into_millis() % 1000,
        log.pid().unwrap_or(0),
        log.tid().unwrap_or(0)
    )?;

    let empty_tags = log.tags().map(|tags| tags.is_empty()).unwrap_or(true);
    if empty_tags {
        write!(writer, "{}", log.component_name())?;
    } else {
        // Unwrap is safe, if we are here it means that we actually have tags.
        let tags = log.tags().unwrap();
        for (i, tag) in tags.iter().enumerate() {
            if denied_tags.contains(tag) {
                return Ok(());
            }
            write!(writer, "{}", tag)?;
            if i < tags.len() - 1 {
                write!(writer, ", ")?;
            }
        }
    }

    write!(writer, "] {}: ", log.severity())?;
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
        let count = writer.write(&data.as_bytes()[offset..])?;
        if offset + count < data.len() {
            pending_str = Some((data, offset + count));
        }
    }
    if !writer.buffer().is_empty() {
        writer.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_data::{BuilderArgs, LogsDataBuilder, LogsField, LogsProperty, Severity};
    use fuchsia_zircon::Time;

    #[derive(Default)]
    struct Test {
        result: Vec<u8>,
    }

    impl SerialResultSink for Test {
        fn write_buffer(&mut self, buf: &[u8]) {
            self.result.extend_from_slice(buf);
        }
    }

    #[fuchsia::test]
    fn create_component_serial_config() {
        let serial_config = SerialConfig::new(vec!["bootstrap/**", "core/foo"], vec!["foo"]);

        let config =
            serial_config.for_component(&ComponentIdentity::from(vec!["bootstrap", "foo"]));
        let config = config.expect("config exists");
        assert_eq!(config.denied_tags.len(), 1);
        assert!(config.denied_tags.contains("foo"));

        assert!(serial_config
            .for_component(&ComponentIdentity::from(vec!["bootstrap", "bar"]))
            .is_some());
        assert!(serial_config
            .for_component(&ComponentIdentity::from(vec!["core", "foo"]))
            .is_some());
        assert!(serial_config
            .for_component(&ComponentIdentity::from(vec!["core", "baz"]))
            .is_none());
    }

    #[fuchsia::test]
    fn write_to_serial_handles_denied_tags() {
        let mut writer = SerialWriter::<Test>::new();
        let log = LogsDataBuilder::new(BuilderArgs {
            timestamp_nanos: Time::from_nanos(1).into(),
            component_url: Some("url".to_string()),
            moniker: "core/foo".to_string(),
            severity: Severity::Info,
        })
        .add_tag("denied-tag")
        .build();
        let denied_tags = HashSet::from_iter(["denied-tag".to_string()].into_iter());
        write_to_serial(log, &denied_tags, &mut writer).expect("Wrote to buffer");
        assert!(writer.sink.result.is_empty());
    }

    #[fuchsia::test]
    fn write_to_serial_splits_lines() {
        let message = concat!(
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Aliquam accumsan eu neque ",
            "quis molestie. Nam rhoncus sapien non eleifend tristique. Duis quis turpis volutpat ",
            "neque bibendum molestie. Etiam ac sapien justo. Nullam aliquet ipsum nec tincidunt."
        );

        let mut writer = SerialWriter::<Test>::new();
        let log = LogsDataBuilder::new(BuilderArgs {
            timestamp_nanos: Time::from_nanos(123456789).into(),
            component_url: Some("url".to_string()),
            moniker: "core/foo".to_string(),
            severity: Severity::Info,
        })
        .add_tag("bar")
        .set_message(message)
        .add_key(LogsProperty::String(LogsField::Other("key".to_string()), "value".to_string()))
        .add_key(LogsProperty::Int(LogsField::Other("other_key".to_string()), 3))
        .set_pid(1234)
        .set_tid(5678)
        .build();
        write_to_serial(log, &HashSet::default(), &mut writer).expect("Wrote to buffer");
        assert_eq!(
            String::from_utf8(writer.sink.result).unwrap(),
            format!(
                "00000.123 01234:05678 [bar] INFO: {}\n{} key=value other_key=3\n",
                &message[..221],
                &message[221..]
            )
        );
    }

    #[fuchsia::test]
    fn when_no_tags_are_present_the_component_name_is_used() {
        let mut writer = SerialWriter::<Test>::new();
        let log = LogsDataBuilder::new(BuilderArgs {
            timestamp_nanos: Time::from_nanos(123456789).into(),
            component_url: Some("url".to_string()),
            moniker: "core/foo".to_string(),
            severity: Severity::Info,
        })
        .set_message("my msg")
        .set_pid(1234)
        .set_tid(5678)
        .build();
        write_to_serial(log, &HashSet::default(), &mut writer).expect("Wrote to buffer");
        assert_eq!(
            String::from_utf8(writer.sink.result).unwrap(),
            "00000.123 01234:05678 [foo] INFO: my msg\n"
        );
    }
}
