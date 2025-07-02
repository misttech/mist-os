// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Reads a system property.
///
/// As this is only expected to be called by ../../src/hal/src/env.rs,
/// we only cover the 3 expected system properties and return an Error
/// on any other property.
pub fn read(name: &str) -> Result<Option<String>, String> {
    match name {
        // The Android properties aren't available by the time the HAL is spawned,
        // but the Fuchsia equivalent properties don't match the KeyMint regexes
        // so resort to hardcoded values that should be kept in sync with the Android
        // prebuilts.
        "ro.build.version.release" => Ok(Some("16".to_string())),
        "ro.build.version.security_patch" => Ok(Some("2025-07-01".to_string())),
        "ro.vendor.build.security_patch" => Ok(Some("2025-07-01".to_string())),
        _ => Err(format!("Unexpected property {}", name)),
    }
}
