// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{stderr, stdout, Write};

use crate::TestBuffers;

/// An object that can be used to produce output, with no support for outputting
/// structured machine-interpretable output.
pub struct Writer {
    stdout: Box<dyn Write>,
    stderr: Box<dyn Write>,
}

impl Writer {
    /// Create a new writer that doesn't support machine output at all, with the
    /// given streams underlying it.
    pub fn new_buffers<'a, O, E>(stdout: O, stderr: E) -> Self
    where
        O: Write + 'static,
        E: Write + 'static,
    {
        let stdout = Box::new(stdout);
        let stderr = Box::new(stderr);
        Self { stdout, stderr }
    }

    /// Create a new Writer that doesn't support machine output at all
    pub fn new() -> Self {
        Self::new_buffers(Box::new(stdout()), Box::new(stderr()))
    }

    /// Returns a writer backed by string buffers that can be extracted after
    /// the writer is done with
    pub fn new_test(test_writer: &TestBuffers) -> Self {
        Self::new_buffers(test_writer.stdout.clone(), test_writer.stderr.clone())
    }

    pub fn stderr(&mut self) -> &'_ mut Box<dyn Write> {
        &mut self.stderr
    }
}

impl Write for Writer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stdout.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stdout.flush()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_not_machine_is_ok() {
        let test_buffers = TestBuffers::default();
        let mut writer = Writer::new_test(&test_buffers);
        let res = writer.item(&"ehllo".to_owned());
        assert!(res.is_ok());
    }

    #[test]
    fn test_item_for_test() {
        let test_buffers = TestBuffers::default();
        let mut writer = Writer::new_test(&test_buffers);
        writer.item(&"hello".to_owned()).unwrap();

        assert_eq!(test_buffers.into_stdout_str(), "hello\n");
    }

    #[test]
    fn test_is_machine_false() {
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(&test_buffers);
        assert!(!writer.is_machine());
    }

    #[test]
    fn line_writer_for_machine_is_ok() {
        let test_buffers = TestBuffers::default();
        let mut writer = Writer::new_test(&test_buffers);
        writer.line("hello").unwrap();

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "hello\n");
        assert_eq!(stderr, "");
    }

    #[test]
    fn writer_print_output_has_no_newline() {
        let test_buffers = TestBuffers::default();
        let mut writer = Writer::new_test(&test_buffers);
        writer.print("foobar").unwrap();

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "foobar");
        assert_eq!(stderr, "");
    }

    #[test]
    fn writer_implements_write() {
        let test_buffers = TestBuffers::default();
        let mut writer = Writer::new_test(&test_buffers);
        writer.write_all(b"foobar").unwrap();

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "foobar");
        assert_eq!(stderr, "");
    }

    #[test]
    fn writing_errors_goes_to_the_right_stream() {
        let test_buffers = TestBuffers::default();
        let mut writer = Writer::new_test(&test_buffers);
        writeln!(writer.stderr(), "hello").unwrap();

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, "hello\n");
    }
}
