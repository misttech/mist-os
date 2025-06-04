// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(remote = "fidl_fuchsia_memory_attribution_plugin::PerformanceImpactMetrics")]
pub struct PerformanceImpactMetricsDef {
    pub some_memory_stalls_ns: Option<i64>,
    pub full_memory_stalls_ns: Option<i64>,
    #[doc(hidden)]
    #[serde(skip)]
    pub __source_breaking: fidl::marker::SourceBreaking,
}
