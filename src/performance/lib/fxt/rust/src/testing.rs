// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Basic utility for emitting fxt records for testing.
#[derive(Clone)]
pub(crate) struct FxtBuilder<H> {
    header: H,
    buf: Vec<u8>,
}

impl<H: crate::header::TraceHeader> FxtBuilder<H> {
    /// Start a new fxt record with a typed header. The header should be completely configured for
    /// the test except for its size in words which will be updated by the builder.
    pub fn new(mut header: H) -> Self {
        // Make space for our header word before anything gets added.
        let mut buf = vec![];
        buf.resize(8, 0);

        // Set an initial size, we'll update as we go.
        header.set_size_words(1);

        Self { header, buf }
    }

    pub fn atom(mut self, atom: impl AsRef<[u8]>) -> Self {
        self.buf.extend(atom.as_ref());
        for _ in 0..crate::word_padding(self.buf.len()) {
            self.buf.push(0);
        }
        assert_eq!(self.buf.len() % 8, 0, "buffer should be word-aligned after adding padding");
        assert!(self.buf.len() < 32_768, "maximum record size is 32kb");
        let size_words: u16 =
            (self.buf.len() / 8).try_into().expect("trace records size in words must fit in a u16");
        self.header.set_size_words(size_words);
        self
    }

    /// Return the bytes of a possibly-valid fxt record with the header in place.
    pub fn build(mut self) -> Vec<u8> {
        self.buf[..8].copy_from_slice(&self.header.to_le_bytes());
        self.buf
    }
}

impl<H: std::fmt::Debug> std::fmt::Debug for FxtBuilder<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Print in word-aligned chunks, exclude the zeroes we keep for the header.
        let chunks = self.buf.chunks_exact(8).skip(1).collect::<Vec<_>>();
        f.debug_struct("FxtBuilder").field("header", &self.header).field("buf", &chunks).finish()
    }
}

#[doc(hidden)]
#[macro_export]
macro_rules! __assert_parses_exactly {
    ($buf:expr => $parser:path => $expected:expr $(,)?) => {{
        use std::fmt::Write;
        const TEST_SENTINEL: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];

        let mut with_sentinel = $buf.to_owned();
        with_sentinel.extend(TEST_SENTINEL);
        let (trailing, observed) = match $parser(&with_sentinel) {
            Ok(p) => p,
            Err(e) => {
                let parser = stringify!($parser);
                let mut message = format!("{parser} failed to parse: {e:?}. buffer contained [");
                for chunk in with_sentinel.chunks(8) {
                    message.push_str("\n    ");
                    for byte in chunk {
                        write!(&mut message, "{byte:#04X}, ").unwrap();
                    }
                }
                message.push_str("\n]");
                panic!("{message}");
            }
        };
        assert_eq!(observed, $expected);
        assert_eq!(
            trailing, TEST_SENTINEL,
            "parser returned the right value but the wrong trailing bytes",
        );
    }};
}

#[macro_export]
macro_rules! assert_parses_to_record {
    ($buf:expr, $expected:expr $(,)?) => {{
        let buf = $buf.to_owned();
        __assert_parses_exactly!(buf => $crate::RawTraceRecord::parse =>
            $crate::ParsedWithOriginalBytes { parsed: $expected, bytes: &buf[..] });
    }};
}

#[macro_export]
macro_rules! assert_parses_to_arg {
    ($buf:expr, $expected:expr $(,)?) => {{
        __assert_parses_exactly!($buf => crate::args::RawArg::parse => $expected);
    }}
}
