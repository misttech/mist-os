// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[fuchsia::main]
async fn main() {
    tracing::debug!("debugging world");
    tracing::info!("Hello, world!");
}
