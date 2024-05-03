// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the kernel area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlatformKernelConfig {
    #[serde(default)]
    pub memory_compression: bool,
    #[serde(default)]
    pub lru_memory_compression: bool,
    #[serde(default)]
    pub continuous_eviction: bool,
    /// For address spaces that use ASLR this controls the number of bits of
    /// entropy in the randomization. Higher entropy results in a sparser
    /// address space and uses more memory for page tables. Valid values range
    /// from 0-36. Default value is 30.
    pub aslr_entropy_bits: Option<u8>,
}
