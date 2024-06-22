// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Amount of memory.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemorySize {
    /// Number of bytes.
    Fixed(u64),
    /// Percentage of the system's physical memory.
    /// the value must be stritly positive, and less than 100.
    Percent(u8),
}

/// Platform configuration options for sysmem. This config exists in both board
/// and platform configs, to allow board config to override static defaults,
/// and to allow platform config to override board config.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlatformSysmemConfig {
    /// Overrides the board-driver-specified size for sysmem's contiguous memory
    /// pool. The default value is 5% if not set by board or platform config.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub contiguous_memory_size: Option<MemorySize>,

    /// Overrides the board-driver-specified size for sysmem's default protected
    /// memory pool. The default value is 0 if not set by board or platform
    /// config.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub protected_memory_size: Option<MemorySize>,

    // If true, sysmem will treat a fraction of currently-unused pages as guard
    // pages and attempt to loan the rest back to zircon. If false, sysmem will
    // attempt to loan all currently-unused pages back to zircon.
    //
    // Enabling this will enable periodic timers in sysmem which check unused
    // pages for stray DMA writes. The default is false to avoid the periodic
    // timers by default. When true, on detection of an improperly written page,
    // sysmem will attempt to log debug info re. allocations that previously
    // used the page.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub contiguous_guard_pages_unused: Option<bool>,
}
