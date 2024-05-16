// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Amount of memory.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemorySize {
    /// Number of bytes.
    Fixed(u64),
    /// Percentage of the system's physical memory.
    /// the value must be stritly positive, and less than 100.
    Relative(u8),
}

/// Platform configuration options for the kernel area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlatformKernelConfig {
    #[serde(default)]
    pub memory_compression: bool,
    #[serde(default)]
    pub lru_memory_compression: bool,
    /// Configures kernel eviction to run continually in the background to try
    /// and keep the system out of memory pressure, as opposed to triggering
    /// one-shot eviction only at memory pressure level transitions.
    /// Enables the `kernel_evict_continuous` assembly input bundle.
    #[serde(default)]
    pub continuous_eviction: bool,
    /// For address spaces that use ASLR this controls the number of bits of
    /// entropy in the randomization. Higher entropy results in a sparser
    /// address space and uses more memory for page tables. Valid values range
    /// from 0-36. Default value is 30.
    pub aslr_entropy_bits: Option<u8>,
    /// Overrides the board-driver-specified size for sysmem's contiguous memory
    /// pool. Default value is 0.
    pub sysmem_contiguous_memory_size: Option<MemorySize>,
    /// Overrides the board-driver-specified size for sysmem's default protected
    /// memory pool. Default value is 5%.
    pub sysmem_protected_memory_size: Option<MemorySize>,
}
