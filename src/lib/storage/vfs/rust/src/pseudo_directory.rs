// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A macro to generate pseudo directory trees using a small DSL.

use fuchsia_zircon::Status;

/// A helper function used by the `pseudo_directory!` macro, to report nice errors in case
/// add_entry() fails.
#[doc(hidden)]
pub fn unwrap_add_entry_span(entry: &str, location: &str, res: Result<(), Status>) {
    if res.is_ok() {
        return;
    }

    let status = res.unwrap_err();
    let text;
    let error_text = match status {
        Status::ALREADY_EXISTS => "Duplicate entry name.",
        _ => {
            text = format!("`add_entry` failed with an unexpected status: {}", status);
            &text
        }
    };

    panic!(
        "Pseudo directory tree generated via pseudo_directory! macro\n\
         {}\n\
         {}\n\
         Entry: '{}'",
        location, error_text, entry
    );
}
