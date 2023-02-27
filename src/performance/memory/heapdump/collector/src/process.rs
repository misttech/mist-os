// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use fuchsia_zircon::Koid;

/// An instrumented process.
#[async_trait]
pub trait Process: Send + Sync {
    /// Returns the cached name of the process.
    fn get_name(&self) -> &str;

    /// Returns the koid of the process.
    fn get_koid(&self) -> Koid;

    /// Serves requests from the process and returns when the process disconnects.
    async fn serve_until_exit(&self) -> Result<(), anyhow::Error>;
}
