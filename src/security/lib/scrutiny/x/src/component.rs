// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::api;
use cm_rust::ComponentDecl;
use std::rc::Rc;

pub(crate) struct Component(Rc<ComponentData>);

impl Component {
    pub fn new(package: Box<dyn api::Package>, manifest: ComponentDecl) -> Self {
        Self(Rc::new(ComponentData { package, manifest }))
    }
}

impl api::Component for Component {
    fn package(&self) -> Box<dyn api::Package> {
        self.0.package.clone()
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
    package: Box<dyn api::Package>,

    // TODO(111243): Use `manifest` to determine return values of `api::Component` methods.
    #[allow(dead_code)]
    manifest: ComponentDecl,
}

#[cfg(test)]
mod tests {
    use super::super::api;
    use super::super::api::Component as _;
    use super::super::package::test::placeholder_package;
    use super::Component;
    use cm_rust::FidlIntoNative;
    use fidl_fuchsia_component_decl as fdecl;

    #[fuchsia::test]
    fn package() {
        let package: Box<dyn api::Package> = Box::new(placeholder_package().clone());
        let manifest = fdecl::Component::default();

        let component = Component::new(package.clone(), manifest.fidl_into_native());

        assert_eq!(package.as_ref(), component.package().as_ref());
    }
}
