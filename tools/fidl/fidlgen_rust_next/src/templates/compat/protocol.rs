// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use crate::id::IdExt as _;
use crate::ir::Protocol;
use crate::templates::{escape, Context, Contextual, Denylist};

use super::{escape_compat, CompatTemplate};

#[derive(Template)]
#[template(path = "compat/protocol.askama")]
pub struct ProtocolCompatTemplate<'a> {
    protocol: &'a Protocol,
    compat: &'a CompatTemplate<'a>,

    name: String,
    proxy_name: String,
    compat_name: String,
    denylist: Denylist,
}

impl<'a> Contextual<'a> for ProtocolCompatTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.compat.context()
    }
}

impl<'a> ProtocolCompatTemplate<'a> {
    pub fn new(protocol: &'a Protocol, compat: &'a CompatTemplate<'a>) -> Self {
        let base_name = protocol.name.decl_name().camel();
        let proxy_name = format!("{base_name}Proxy");
        let compat_name =
            format!("{}Marker", escape_compat(base_name.clone(), protocol.name.decl_name()),);

        Self {
            protocol,
            compat,

            name: escape(base_name),
            proxy_name,
            compat_name,
            denylist: compat.rust_or_rust_next_denylist(&protocol.name),
        }
    }
}
