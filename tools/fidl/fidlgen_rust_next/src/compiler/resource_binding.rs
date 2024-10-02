// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub struct ResourceBinding {
    pub wire_path: String,
    pub optional_wire_path: String,
    pub natural_path: String,
}

pub struct ResourceBindings {
    pub handle: ResourceBinding,
    pub server_end: ResourceBinding,
    pub client_end: ResourceBinding,
}

impl Default for ResourceBindings {
    fn default() -> Self {
        Self {
            handle: ResourceBinding {
                wire_path: "::fidl_next::WireHandle".to_string(),
                optional_wire_path: "::fidl_next::OptionalWireHandle".to_string(),
                natural_path: "::fidl_next::Handle".to_string(),
            },
            server_end: ResourceBinding {
                wire_path: "::fidl_next::WireServerEnd".to_string(),
                optional_wire_path: "::fidl_next::OptionalWireServerEnd".to_string(),
                natural_path: "::fidl_next::ServerEnd".to_string(),
            },
            client_end: ResourceBinding {
                wire_path: "::fidl_next::WireClientEnd".to_string(),
                optional_wire_path: "::fidl_next::OptionalWireClientEnd".to_string(),
                natural_path: "::fidl_next::ClientEnd".to_string(),
            },
        }
    }
}
