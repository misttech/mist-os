// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::api;
use cm_rust::ComponentDecl;
use std::rc::Rc;

pub(crate) struct Component(Rc<ComponentData>);

impl Component {
    pub fn new(manifest: ComponentDecl) -> Self {
        Self(Rc::new(ComponentData { manifest }))
    }
}

impl api::Component for Component {
    fn packages(&self) -> Box<dyn Iterator<Item = Box<dyn api::Package>>> {
        todo!("TODO(111243): Store sufficient information in components to lookup all packages that contain component");
    }

    fn children(&self) -> Box<dyn Iterator<Item = api::PackageResolverUrl>> {
        todo!("TODO(111243): Return children declared in manifest");
    }

    fn uses(&self) -> Box<dyn Iterator<Item = Box<dyn api::ComponentCapability>>> {
        todo!("TODO(111243): Return uses declared in manifest");
    }

    fn exposes(&self) -> Box<dyn Iterator<Item = Box<dyn api::ComponentCapability>>> {
        todo!("TODO(111243): Return exposes declared in manifest");
    }

    fn offers(&self) -> Box<dyn Iterator<Item = Box<dyn api::ComponentCapability>>> {
        todo!("TODO(111243): Return offers declared in manifest");
    }

    fn capabilities(&self) -> Box<dyn Iterator<Item = Box<dyn api::ComponentCapability>>> {
        todo!("TODO(111243): Return capabilities declared in manifest");
    }

    fn instances(&self) -> Box<dyn Iterator<Item = Box<dyn api::ComponentInstance>>> {
        todo!("TODO(111243): Return instances declared in manifest");
    }
}

struct ComponentData {
    // TODO(111243): Use `manifest` to determine return values of `api::Component` methods.
    #[allow(dead_code)]
    manifest: ComponentDecl,
}

#[cfg(test)]
mod tests {
    // TODO(111243): Write tests as `api::Component` methods are implemented.
}
