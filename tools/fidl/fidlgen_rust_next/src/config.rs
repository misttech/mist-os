// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::ir::HandleSubtype;

pub struct Config {
    pub emit_compat: bool,
    pub emit_debug_impls: bool,
    pub resource_bindings: ResourceBindings,
}

pub struct HandleResourceBinding {
    wire_path: String,
    optional_wire_path: String,
    natural_path: String,
}

impl HandleResourceBinding {
    fn handle_subtype_name(subtype: HandleSubtype) -> &'static str {
        match subtype {
            HandleSubtype::None => "Handle",
            HandleSubtype::Process => "Process",
            HandleSubtype::Thread => "Thread",
            HandleSubtype::Vmo => "Vmo",
            HandleSubtype::Channel => "Channel",
            HandleSubtype::Event => "Event",
            HandleSubtype::Port => "Port",
            HandleSubtype::Interrupt => "Interrupt",
            HandleSubtype::PciDevice => "PciDevice",
            HandleSubtype::Log => "Log",
            HandleSubtype::Socket => "Socket",
            HandleSubtype::Resource => "Resource",
            HandleSubtype::EventPair => "EventPair",
            HandleSubtype::Job => "Job",
            HandleSubtype::Vmar => "Vmar",
            HandleSubtype::Fifo => "Fifo",
            HandleSubtype::Guest => "Guest",
            HandleSubtype::Vcpu => "Vcpu",
            HandleSubtype::Timer => "Timer",
            HandleSubtype::Iommu => "Iommu",
            HandleSubtype::Bti => "Bti",
            HandleSubtype::Profile => "Profile",
            HandleSubtype::Pmt => "Pmt",
            HandleSubtype::SuspendToken => "SuspendToken",
            HandleSubtype::Pager => "Pager",
            HandleSubtype::Exception => "Exception",
            HandleSubtype::Clock => "Clock",
            HandleSubtype::Stream => "Stream",
            HandleSubtype::Msi => "Msi",
            HandleSubtype::Iob => "Iob",
        }
    }

    pub fn wire_path(&self, subtype: HandleSubtype) -> String {
        self.wire_path.replace("{subtype}", Self::handle_subtype_name(subtype))
    }

    pub fn optional_wire_path(&self, subtype: HandleSubtype) -> String {
        self.optional_wire_path.replace("{subtype}", Self::handle_subtype_name(subtype))
    }

    pub fn natural_path(&self, subtype: HandleSubtype) -> String {
        self.natural_path.replace("{subtype}", Self::handle_subtype_name(subtype))
    }
}

pub struct ResourceBindings {
    pub handle: HandleResourceBinding,
}

impl Default for ResourceBindings {
    fn default() -> Self {
        Self {
            handle: HandleResourceBinding {
                wire_path: "::fidl_next::fuchsia::Wire{subtype}".to_string(),
                optional_wire_path: "::fidl_next::fuchsia::WireOptional{subtype}".to_string(),
                natural_path: "::fidl_next::fuchsia::zx::{subtype}".to_string(),
            },
        }
    }
}
