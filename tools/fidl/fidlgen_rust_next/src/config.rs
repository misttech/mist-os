// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub struct Config {
    pub emit_debug_impls: bool,
    pub resource_bindings: ResourceBindings,
}

pub struct ResourceBinding {
    pub wire_path: String,
    pub optional_wire_path: String,
    #[allow(dead_code)]
    pub natural_path: String,
}

pub struct ResourceBindings {
    pub handle: ResourceBinding,
}

impl Default for ResourceBindings {
    fn default() -> Self {
        Self {
            handle: ResourceBinding {
                wire_path: "::fidl_next::WireHandle".to_string(),
                optional_wire_path: "::fidl_next::WireOptionalHandle".to_string(),
                natural_path: "::fidl_next::zx::Handle".to_string(),
            },
        }
    }
}
