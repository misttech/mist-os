// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::outcome::ConnectionError, fidl_fuchsia_test_manager::RunBuilderProxy,
    futures::lock::Mutex,
};

/// Implementing this trait allows configuring the number of suites to
/// run on a single RunBuilder connection.
/// This alleviates an issue where for n suites run on a single RunBuilder
/// connection, n channels must be opened up front. This can cause some
/// issues with resource limitations when a large number of tests is
/// specified (see https://fxbug.dev/42062444).
#[async_trait::async_trait]
pub trait RunBuilderConnector {
    /// Create a new connection to RunBuilder.
    async fn connect(&self) -> Result<RunBuilderProxy, ConnectionError>;
    /// Number of suites for which a connection produced by this connector
    /// should be used.
    fn batch_size(&self) -> usize;
}

/// A connector that produces a single proxy and instructs all suites to
/// be executed using it.
pub struct SingleRunConnector {
    proxy: Mutex<Option<RunBuilderProxy>>,
}

impl SingleRunConnector {
    pub fn new(proxy: RunBuilderProxy) -> Self {
        Self { proxy: Mutex::new(Some(proxy)) }
    }
}

#[async_trait::async_trait]
impl RunBuilderConnector for SingleRunConnector {
    async fn connect(&self) -> Result<RunBuilderProxy, ConnectionError> {
        Ok(self.proxy.lock().await.take().expect("connect only called once"))
    }

    fn batch_size(&self) -> usize {
        usize::MAX
    }
}
