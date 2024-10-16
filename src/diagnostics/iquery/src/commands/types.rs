// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::types::Error;
use async_trait::async_trait;
use diagnostics_data::{Data, DiagnosticsData};
use serde::Serialize;
use std::fmt::Display;

#[async_trait]
pub trait Command {
    type Result: Serialize + Display;
    async fn execute<P: DiagnosticsProvider>(&self, provider: &P) -> Result<Self::Result, Error>;
}

#[async_trait]
pub trait DiagnosticsProvider: Send + Sync {
    async fn snapshot<D: DiagnosticsData>(
        &self,
        accessor: &Option<String>,
        selectors: &[String],
    ) -> Result<Vec<Data<D>>, Error>;

    /// Lists all ArchiveAccessor selectors.
    async fn get_accessor_paths(&self) -> Result<Vec<String>, Error>;
}
