// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Reads a system property.
///
/// Returns `Ok(None)` if the property doesn't exist.
pub fn read(_name: &str) -> Result<Option<String>, String> {
    // TODO(https://fxbug.dev/392914358): return a meaningful value.
    Ok(Some("stub.property.value".to_string()))
}
