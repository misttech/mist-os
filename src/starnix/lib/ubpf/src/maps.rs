// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use linux_uapi::bpf_map_type;
use std::ops::Range;

#[derive(Debug, Clone, Copy)]
pub struct MapSchema {
    pub map_type: bpf_map_type,
    pub key_size: u32,
    pub value_size: u32,
    pub max_entries: u32,
}

impl MapSchema {
    pub fn array_range_for_index(&self, index: u32) -> Range<usize> {
        let base = index * self.value_size;
        let limit = base + self.value_size;
        (base as usize)..(limit as usize)
    }
}
