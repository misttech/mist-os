// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::component_model::AnalyzerModelError,
    cm_types::Name,
    moniker::Moniker,
    routing::mapper::RouteSegment,
    serde::{Deserialize, Serialize},
    thiserror::Error,
};

/// A summary of a specific capability route and the outcome of verification.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifyRouteResult {
    /// TODO(https://fxbug.dev/42053778): Rename to `moniker`.
    pub using_node: Moniker,
    pub capability: Option<Name>,
    pub error: Option<AnalyzerModelError>,
    pub route: Vec<RouteSegment>,
}

#[derive(Clone, Debug, Deserialize, Error, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityRouteError {
    #[error(transparent)]
    AnalyzerModelError(#[from] AnalyzerModelError),
}
