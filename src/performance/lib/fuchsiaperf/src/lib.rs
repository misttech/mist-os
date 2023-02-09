// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// A struct that can be json serialized to the fuchsiaperf.json format.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct FuchsiaPerfBenchmarkResult {
    pub label: String,
    pub test_suite: String,
    pub unit: String,
    pub values: Vec<f64>,
}
