// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::helpers::MapsContext;
use super::maps::MapValueRef;

#[derive(Default)]
pub struct BaseEbpfRunContext<'a> {
    map_refs: Vec<MapValueRef<'a>>,
}

impl<'a> MapsContext<'a> for BaseEbpfRunContext<'a> {
    fn add_value_ref(&mut self, map_ref: MapValueRef<'a>) {
        self.map_refs.push(map_ref)
    }
}
