// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;
use std::ops::Deref;

#[derive(Copy, Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ProjectId(pub u32);

impl Deref for ProjectId {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Copy, Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct CustomerId(pub u32);

impl Default for CustomerId {
    fn default() -> Self {
        CustomerId(1)
    }
}

impl Deref for CustomerId {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Copy, Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct MetricId(pub u32);

impl Deref for MetricId {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Copy, Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct EventCode(pub u32);

impl Deref for EventCode {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Supported Cobalt Metric types
#[derive(Deserialize, Debug, PartialEq, Eq, Copy, Clone)]
pub enum MetricType {
    /// Maps cached diffs from Uint or Int Inspect types.
    /// NOTE: This does not use duration tracking. Durations are always set to 0.
    Occurrence,

    /// Maps raw Int Inspect types.
    Integer,

    /// Maps cached diffs from IntHistogram Inspect type.
    IntHistogram,

    /// Maps Inspect String type to StringValue (Cobalt 1.1 only).
    String,
}
