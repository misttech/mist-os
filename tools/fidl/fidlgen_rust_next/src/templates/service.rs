// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{filters, Context, Contextual};
use crate::id::IdExt as _;
use crate::ir::Service;
use crate::templates::reserved::escape;

#[derive(Template)]
#[template(path = "service.askama", whitespace = "preserve")]
pub struct ServiceTemplate<'a> {
    service: &'a Service,
    context: Context<'a>,

    non_canonical_name: &'a str,
    service_name: String,
    instance_trait_name: String,
}

impl<'a> ServiceTemplate<'a> {
    pub fn new(service: &'a Service, context: Context<'a>) -> Self {
        let base_name = service.name.decl_name().camel();
        let instance_trait_name = format!("{base_name}Instance");

        Self {
            service,
            context,

            non_canonical_name: service.name.decl_name().non_canonical(),
            service_name: escape(base_name),
            instance_trait_name: escape(instance_trait_name),
        }
    }

    fn service_name(&self) -> String {
        let (library, name) = self.service.name.split();
        format!("{}.{}", library, name.camel())
    }
}

impl<'a> Contextual<'a> for ServiceTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.context
    }
}
