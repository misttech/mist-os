// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use std::io::{stderr, stdout, Write};
use std::ops::DerefMut;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{format_output, Format};

/// This is the legacy writer that doesn't impose any particular type of the
/// machine output. It should not be used in new code.
// TODO(mgnb): Remove after all uses have been removed.
#[derive(Debug, Clone, Default)]
pub struct Writer {
    format: Option<Format>,
    test_buffer: Buffer,
    test_error_buffer: Buffer,
}

impl Writer {
    /// Create a new Writer with the specified format.
    ///
    /// Passing None for format implies no output via the machine function.
    pub fn new(format: Option<Format>) -> Self {
        Self { format, ..Default::default() }
    }

    /// Create a new Writer with the specified format that captures all output for later replay.
    ///
    /// Passing None for format implies no output via the machine function.
    pub fn new_test(format: Option<Format>) -> Self {
        Self { format, test_buffer: Buffer::empty(), test_error_buffer: Buffer::empty() }
    }

    /// Get all output that would have been generated on standard output by this object.
    ///
    /// An error will be returned if this method is called on a non-test instance.
    pub fn test_output(&self) -> Result<String> {
        self.test_buffer.as_string()
    }

    /// Get all output that would have been generated on standard error by this object.
    ///
    /// An error will be returned if this method is called on a non-test instance.
    pub fn test_error(&self) -> Result<String> {
        self.test_error_buffer.as_string()
    }

    /// Writes machine consumable output to standard output.
    ///
    /// This is a no-op if `is_machine` returns false.
    pub fn machine<T: serde::Serialize>(&self, output: &T) -> Result<()> {
        if let Some(format) = self.format {
            format_output(format, &mut self.inner(), output)?;
        }
        Ok(())
    }

    /// Returns true if the receiver was configured to output for machines.
    pub fn is_machine(&self) -> bool {
        self.format.is_some()
    }

    /// Writes the value to standard output without a newline.
    ///
    /// This is a no-op if `is_machine` returns true.
    pub fn write(&self, value: impl std::fmt::Display) -> Result<()> {
        if self.is_machine() {
            return Ok(());
        }
        Ok(write!(self.inner(), "{}", value)?)
    }

    /// Writes the value to standard output with a newline.
    ///
    /// This is a no-op if `is_machine` returns true.
    pub fn line(&self, value: impl std::fmt::Display) -> Result<()> {
        if self.is_machine() {
            return Ok(());
        }
        Ok(writeln!(self.inner(), "{}", value)?)
    }

    /// Writes the value to standard error with a newline.
    ///
    /// This is output regardless of the value that `is_machine` returns.
    pub fn info(&self, value: impl std::fmt::Display) -> Result<()> {
        Ok(writeln!(self.inner_error(), "{}", value)?)
    }

    /// Writes the value to standard error with a newline.
    ///
    /// This is output regardless of the value that `is_machine` returns.
    pub fn error(&self, value: impl std::fmt::Display) -> Result<()> {
        Ok(writeln!(self.inner_error(), "{}", value)?)
    }

    fn inner(&self) -> InnerWriter<'_, impl Write> {
        InnerWriter(self.test_buffer.0.as_ref().map(|b| b.lock().unwrap()), stdout())
    }

    fn inner_error(&self) -> InnerWriter<'_, impl Write> {
        InnerWriter(self.test_error_buffer.0.as_ref().map(|b| b.lock().unwrap()), stderr())
    }
}

impl std::io::Write for Writer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.is_machine() {
            Ok(buf.len())
        } else {
            self.inner().write(buf)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.is_machine() {
            Ok(())
        } else {
            self.inner().flush()
        }
    }
}

// This uses an Arc<Mutex<_>> to allow for this type to be shared across threads. It is not
// expected that this object is running in a multithreaded executor, but some plugins spawn
// explicit threads. Moreover, this should only be used in test environments so there should be
// zero overhead in production.
#[derive(Debug, Clone)]
struct Buffer(Option<Arc<Mutex<Vec<u8>>>>);

impl Buffer {
    fn empty() -> Self {
        Buffer(Some(Arc::new(Mutex::new(Vec::new()))))
    }

    fn as_string(&self) -> Result<String> {
        self.0
            .as_ref()
            .ok_or(anyhow!("Misconfigured Writer, test buffer is missing"))
            .and_then(|b| String::from_utf8(b.lock().unwrap().clone()).map_err(Into::into))
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self(None)
    }
}

// This is a convenience type to allow the stdout and stderr objects to share a single Write
// implementation.
pub struct InnerWriter<'a, T: Write>(Option<MutexGuard<'a, Vec<u8>>>, T);

impl<'a, T: Write> Write for InnerWriter<'a, T> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.0 {
            Some(ref mut b) => b.deref_mut().write(buf),
            None => self.1.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.0 {
            None => self.1.flush(),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_not_machine_is_ok() {
        let writer = Writer::new(None);
        let res = writer.machine(&"ehllo");
        assert!(res.is_ok());
    }

    #[test]
    fn test_machine_valid_json_is_ok() {
        let writer = Writer::new(Some(Format::Json));
        let res = writer.machine(&"ehllo");
        assert!(res.is_ok());
    }

    #[test]
    fn test_machine_for_test() {
        let writer = Writer::new_test(Some(Format::Json));
        writer.machine(&"hello").unwrap();

        assert_eq!(writer.test_output().unwrap(), "\"hello\"\n");
    }

    #[test]
    fn test_not_machine_for_test_is_empty() {
        let writer = Writer::new_test(None);
        writer.machine(&"hello").unwrap();

        assert!(writer.test_output().unwrap().is_empty());
    }

    #[test]
    fn test_machine_makes_is_machine_true() {
        let writer = Writer::new(Some(Format::Json));
        assert!(writer.is_machine());
    }

    #[test]
    fn test_not_machine_makes_is_machine_false() {
        let writer = Writer::new(None);
        assert!(!writer.is_machine());
    }

    #[test]
    fn line_writer_for_machine_is_ok() {
        let writer = Writer::new_test(Some(Format::Json));
        writer.line("hello").unwrap();

        assert_eq!(writer.test_output().unwrap(), "");
        assert_eq!(writer.test_error().unwrap(), "");
    }

    #[test]
    fn writer_write_for_machine_is_ok() {
        let writer = Writer::new_test(Some(Format::Json));
        writer.write("foobar").unwrap();
        assert_eq!(writer.test_output().unwrap(), "");
        assert_eq!(writer.test_error().unwrap(), "");
    }

    #[test]
    fn writer_write_output_has_no_newline() {
        let writer = Writer::new_test(None);
        writer.write("foobar").unwrap();
        assert_eq!(writer.test_output().unwrap(), "foobar");
        assert_eq!(writer.test_error().unwrap(), "");
    }

    #[test]
    fn writing_errors_goes_to_the_right_stream() {
        let writer = Writer::new_test(None);
        writer.error("hello").unwrap();

        assert_eq!(writer.test_output().unwrap(), "");
        assert_eq!(writer.test_error().unwrap(), "hello\n");
    }

    #[test]
    fn line_writer_to_clone_is_shared() {
        let writer = Writer::new_test(None);
        let writer_clone = writer.clone();

        writer_clone.line("hello").unwrap();

        assert_eq!(writer.test_output().unwrap(), "hello\n");
        assert_eq!(writer.test_error().unwrap(), "");
    }

    #[test]
    fn test_machine_writes_pretty_json() {
        let writer = Writer::new_test(Some(Format::JsonPretty));
        let test_input = serde_json::json!({
            "object1": {
                "line1": "hello",
                "line2": "foobar"
            }
        });
        writer.machine(&test_input).unwrap();

        assert_eq!(
            writer.test_output().unwrap(),
            r#"{
  "object1": {
    "line1": "hello",
    "line2": "foobar"
  }
}"#
        );
    }
}
