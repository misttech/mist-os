// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use linux_uapi::bpf_map_type;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapSchema {
    pub map_type: bpf_map_type,
    pub key_size: u32,
    pub value_size: u32,
    pub max_entries: u32,
}
