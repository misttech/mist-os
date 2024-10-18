// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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
        fn hook(&mut self, _: &[i64], _: &[u8], _: bool) {}
        fn put(&mut self, _: u8) {}
        fn unhook(&mut self) {}
        fn osc_dispatch(&mut self, _: &[&[u8]]) {}
        fn csi_dispatch(&mut self, _: &[i64], _: &[u8], _: bool, _: char) {}
        fn esc_dispatch(&mut self, _: &[i64], _: &[u8], _: bool, _: u8) {}
    }

    let mut counter = VteCounter(0);
    let mut parser = vte::Parser::new();
    s.as_bytes().iter().copied().for_each(|x| parser.advance(&mut counter, x));

    counter.0
}
