// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::component_model::AnalyzerModelError;
use cm_types::Name;
use moniker::Moniker;
use routing::capability_source::CapabilitySource;
use routing::mapper::RouteSegment;
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, PartialEq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetDecl {
    Use(cm_rust::UseDecl),
    Offer(cm_rust::OfferDecl),
    Expose(cm_rust::ExposeDecl),
    ResolverFromEnvironment(String),
}

/// A summary of a specific capability route and the outcome of verification.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifyRouteResult {
    /// TODO(https://fxbug.dev/42053778): Rename to `moniker`.
    pub using_node: Moniker,
    pub target_decl: TargetDecl,
    pub capability: Option<Name>,
    pub error: Option<AnalyzerModelError>,
    pub route: Vec<RouteSegment>,
    pub source: Option<CapabilitySource>,
}
