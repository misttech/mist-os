// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

/// Provides shared memory buffers (in the form of [`TestBuffer`]s for stdout
/// and stderr that can be used with implementations of [`crate::ToolIO`] to
/// test input and output behaviour at runtime.
#[derive(Default, Debug)]
pub struct TestBuffers {
    pub stdout: TestBuffer,
    pub stderr: TestBuffer,
}

/// Provides a shared memory buffer that can be cloned that can be used with
/// implementations of [`crate::ToolIO`] to test input and output behaviour
/// at runtime.
#[derive(Clone, Default, Debug)]
pub struct TestBuffer {
    inner: Rc<RefCell<Vec<u8>>>,
}

impl TestBuffers {
    /// Destroy `self` and return the standard output and error buffers as byte
    /// arrays.
    pub fn into_stdio(self) -> (Vec<u8>, Vec<u8>) {
        let stdout = self.stdout.into_inner();
        let stderr = self.stderr.into_inner();
        (stdout, stderr)
    }

    /// Destroy `self` and return the standard output and error buffers as strings.
    pub fn into_strings(self) -> (String, String) {
        let stdout = self.stdout.into_string();
        let stderr = self.stderr.into_string();
        (stdout, stderr)
    }

    /// Destroy `self` and return the standard output buffer as a byte
    /// array.
    pub fn into_stdout(self) -> Vec<u8> {
        self.stdout.into_inner()
    }

    /// Destroy `self` and return the standard output buffer as a string.
    pub fn into_stdout_str(self) -> String {
        self.stdout.into_string()
    }

    /// Destroy `self` and return the standard error buffer as a byte
    /// array.
    pub fn into_stderr(self) -> Vec<u8> {
        self.stderr.into_inner()
    }

    /// Destroy `self` and return the standard error buffer as a string.
    pub fn into_stderr_str(self) -> String {
        self.stderr.into_string()
    }
}

impl TestBuffer {
    /// Destroys self and returns the inner buffer as a vector.
    pub fn into_inner(self) -> Vec<u8> {
        self.inner.take().into()
    }

    /// Destroys self and returns the inner buffer as a string.
    pub fn into_string(self) -> String {
        String::from_utf8(self.into_inner()).expect("Valid unicode on output string")
    }
}

impl Write for TestBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.borrow_mut().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.borrow_mut().flush()
    }
}
