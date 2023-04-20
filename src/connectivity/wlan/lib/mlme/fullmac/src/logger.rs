// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Once;

static LOGGER_ONCE: Once = Once::new();

pub fn init() {
    // The Fuchsia syslog must not be initialized more than once per process. In the case of MLME,
    // that means we can only call it once for both the client and ap modules. Ensure this by using
    // a shared `Once::call_once()`.
    LOGGER_ONCE.call_once(|| {
        // Initialize logging with a tag that can be used to filter for forwarding to console
        diagnostics_log::init_and_detach_ok!(diagnostics_log::PublishOptions {
            tags: &["wlan"],
            metatags: [diagnostics_log::Metatag::Target].into_iter().collect(),
            ..Default::default()
        });
    });
}
