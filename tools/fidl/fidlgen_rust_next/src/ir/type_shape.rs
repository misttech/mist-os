// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct TypeShape {
    #[expect(dead_code)]
    pub alignment: u32,
    #[expect(dead_code)]
    pub depth: u32,
    #[expect(dead_code)]
    pub has_flexible_envelope: bool,
    #[expect(dead_code)]
    pub has_padding: bool,
    #[expect(dead_code)]
    pub inline_size: u32,
    #[expect(dead_code)]
    pub max_handles: u32,
    pub max_out_of_line: u32,
}
