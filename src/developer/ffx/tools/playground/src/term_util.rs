// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use vte::{Params, Parser};

/// Get the length of a string when printed to the terminal (ignoring control
/// sequences).
///
/// This just strips sequences out. It doesn't account for how cursor movements
/// might change string length.
pub fn printed_length(s: &str) -> usize {
    struct VteCounter(usize);

    impl vte::Perform for VteCounter {
        fn print(&mut self, c: char) {
            self.0 += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        }
        fn execute(&mut self, _: u8) {}
        fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
        fn put(&mut self, _: u8) {}
        fn unhook(&mut self) {}
        fn osc_dispatch(&mut self, _: &[&[u8]], _: bool) {}
        fn csi_dispatch(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
        fn esc_dispatch(&mut self, _: &[u8], _: bool, _: u8) {}
    }

    let mut counter = VteCounter(0);
    let mut parser = Parser::new();
    parser.advance(&mut counter, &s.as_bytes());

    counter.0
}
