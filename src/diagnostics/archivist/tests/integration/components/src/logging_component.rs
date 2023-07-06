// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#[fuchsia::main(logging_tags = ["logging component"], logging_minimum_severity = "debug")]
async fn main() {
    tracing::debug!("my debug message.");
    tracing::info!("my info message.");
    tracing::warn!("my warn message.");
}
