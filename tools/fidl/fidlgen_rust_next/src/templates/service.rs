// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{doc_string, filters, Context};
use crate::id::IdExt as _;
use crate::ir::Service;

#[derive(Template)]
#[template(path = "service.askama", whitespace = "preserve")]
pub struct ServiceTemplate<'a> {
    service: &'a Service,
    context: &'a Context,
}

impl<'a> ServiceTemplate<'a> {
    pub fn new(service: &'a Service, context: &'a Context) -> Self {
        Self { service, context }
    }

    fn service_name(&self) -> String {
        let (library, name) = self.service.name.split();
        format!("{}.{}", library, name.camel())
    }
}
