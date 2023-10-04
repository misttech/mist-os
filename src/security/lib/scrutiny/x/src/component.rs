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

    fn declaration(&self) -> ComponentDecl {
        self.0.manifest.clone()
    }
}

struct ComponentData {
    package: Box<dyn api::Package>,
    manifest: ComponentDecl,
}

#[cfg(test)]
pub(crate) mod test {
    use super::super::api;
    use super::super::hash::Hash;
    use cm_rust as cm;
    use cm_rust::NativeIntoFidl as _;
    use cm_rust_testing as cmt;

    const PLACEHOLDER_COMPONENT_PATH: &str = "meta/placeholder.cm";
    const PLACEHOLDER_CHILD_URL: &str = "#meta/test_child.cm";

    pub(crate) fn placeholder_component_path() -> String {
        PLACEHOLDER_COMPONENT_PATH.to_string()
    }

    pub(crate) fn placeholder_component() -> cm::ComponentDecl {
        cmt::ComponentDeclBuilder::new()
            .add_child(cmt::ChildDeclBuilder::new().url(PLACEHOLDER_CHILD_URL).build())
            .use_(cm::UseDecl::Protocol(cm::UseProtocolDecl {
                dependency_type: cm::DependencyType::Strong,
                source: cm::UseSource::Parent,
                source_name: "test_protocol".parse().expect("use protocol name"),
                target_path: "/svc/test_protocol".parse().expect("use protocol target path"),
                availability: cm::Availability::Required,
            }))
            .expose(cm::ExposeDecl::Protocol(cm::ExposeProtocolDecl {
                source: cm::ExposeSource::Self_,
                source_name: "test_protocol".parse().expect("expose protocol source name"),
                target_name: "test_protocol".parse().expect("expose protocol target name"),
                target: cm::ExposeTarget::Parent,
                availability: cm::Availability::Required,
            }))
            .offer(cm::OfferDecl::Protocol(cm::OfferProtocolDecl {
                source: cm::OfferSource::Parent,
                source_name: "test_protocol".parse().unwrap(),
                target: cm::OfferTarget::static_child("test_child".to_string()),
                target_name: "test_protocol".parse().unwrap(),
                dependency_type: cm::DependencyType::Strong,
                availability: cm::Availability::Required,
            }))
            .build()
    }

    pub(crate) fn placeholder_component_cm() -> (Box<dyn api::Hash>, Vec<u8>) {
        let component = placeholder_component();
        let component_fidl = component.native_into_fidl();
        let component_bytes = fidl::persist(&component_fidl).expect("persist component fidl");
        let component_hash = Hash::from_contents(component_bytes.as_slice());
        (Box::new(component_hash), component_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::super::api;
    use super::super::api::Component as _;
    use super::super::package::test::placeholder_package;
    use super::test::placeholder_component;
    use super::Component;
    use cm_rust::FidlIntoNative as _;
    use fidl_fuchsia_component_decl as fdecl;

    #[fuchsia::test]
    fn package() {
        let package: Box<dyn api::Package> = Box::new(placeholder_package().clone());
        let manifest = fdecl::Component::default();

        let component = Component::new(package.clone(), manifest.fidl_into_native());

        assert_eq!(package.as_ref(), component.package().as_ref());
    }

    #[fuchsia::test]
    fn declaration() {
        let package: Box<dyn api::Package> = Box::new(placeholder_package().clone());

        let component = Component::new(package, placeholder_component());

        assert_eq!(component.declaration(), placeholder_component());
    }
}
