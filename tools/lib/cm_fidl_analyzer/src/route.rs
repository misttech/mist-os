// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{component_model::AnalyzerModelError, node_path::NodePath},
    cm_types::Name,
    routing::mapper::RouteSegment,
    serde::{Deserialize, Serialize},
    thiserror::Error,
};

/// A summary of a specific capability route and the outcome of verification.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifyRouteResult {
    pub using_node: NodePath,
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
