// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::path::PathBuf;

pub struct HostDriver {}

impl HostDriver {
    pub async fn run(_socket_path: PathBuf) {}
}
