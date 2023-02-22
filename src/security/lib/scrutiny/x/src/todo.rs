// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::blob::Blob;
use crate::hash::Hash;
///!
///! Placeholder trait implementations used for gradual integration of production implementations.
///!
///! When a new production implementation is ready for integration with the non-test build
///! environment, its placeholder implementation should be removed from this module, and the
///! production implementation appropriately integrated with other production implementations.
///!
// Production implementations in active use by `Scrutiny`.
use crate::package::ScrutinyPackage;

// APIs to be stubbed.
use crate::api::CapabilityDestination;
use crate::api::CapabilityKind;
use crate::api::CapabilitySource;
use crate::api::Component as ComponentApi;
use crate::api::ComponentCapability as ComponentCapabilityApi;
use crate::api::ComponentCapabilityName as ComponentCapabilityNameApi;
use crate::api::ComponentCapabilityPath as ComponentCapabilityPathApi;
use crate::api::ComponentInstance as ComponentInstanceApi;
use crate::api::ComponentInstanceCapability as ComponentInstanceCapabilityApi;
use crate::api::ComponentManager as ComponentManagerApi;
use crate::api::ComponentManagerConfiguration as ComponentManagerConfigurationApi;
use crate::api::ComponentResolver as ComponentResolverApi;
use crate::api::ComponentResolverUrl;
use crate::api::DevMgrConfiguration as DevMgrConfigurationApi;
use crate::api::Environment as EnvironmentApi;
use crate::api::KernelFlags as KernelFlagsApi;
use crate::api::Moniker as MonikerApi;
use crate::api::PackageResolver as PackageResolverApi;
use crate::api::PackageResolverUrl;
use crate::api::System as SystemApi;
use crate::api::VbMeta as VbMetaApi;
use crate::api::Zbi as ZbiApi;

// External dependencies.
use std::path::PathBuf;

pub struct ComponentInstanceCapability;

impl ComponentInstanceCapabilityApi for ComponentInstanceCapability {
    type ComponentCapability = ComponentCapability;
    type ComponentInstance = ComponentInstance;

    fn component_capability(&self) -> Self::ComponentCapability {
        todo!(
            "TODO(fxbug.dev/111246): Integrate with production component instance capability API"
        );
    }

    fn component_instance(&self) -> Self::ComponentInstance {
        todo!(
            "TODO(fxbug.dev/111246): Integrate with production component instance capability API"
        );
    }

    fn source(
        &self,
    ) -> Box<
        dyn ComponentInstanceCapabilityApi<
            ComponentCapability = Self::ComponentCapability,
            ComponentInstance = Self::ComponentInstance,
        >,
    > {
        todo!(
            "TODO(fxbug.dev/111246): Integrate with production component instance capability API"
        );
    }

    fn source_path(
        &self,
    ) -> Box<
        dyn Iterator<
            Item = Box<
                dyn ComponentInstanceCapabilityApi<
                    ComponentCapability = Self::ComponentCapability,
                    ComponentInstance = Self::ComponentInstance,
                >,
            >,
        >,
    > {
        todo!(
            "TODO(fxbug.dev/111246): Integrate with production component instance capability API"
        );
    }

    fn destination_paths(
        &self,
    ) -> Box<
        dyn Iterator<
            Item = Box<
                dyn Iterator<
                    Item = Box<
                        dyn ComponentInstanceCapabilityApi<
                            ComponentCapability = Self::ComponentCapability,
                            ComponentInstance = Self::ComponentInstance,
                        >,
                    >,
                >,
            >,
        >,
    > {
        todo!(
            "TODO(fxbug.dev/111246): Integrate with production component instance capability API"
        );
    }

    fn all_paths(
        &self,
    ) -> Box<
        dyn Iterator<
            Item = Box<
                dyn Iterator<
                    Item = Box<
                        dyn ComponentInstanceCapabilityApi<
                            ComponentCapability = Self::ComponentCapability,
                            ComponentInstance = Self::ComponentInstance,
                        >,
                    >,
                >,
            >,
        >,
    > {
        todo!(
            "TODO(fxbug.dev/111246): Integrate with production component instance capability API"
        );
    }
}

pub struct ComponentManager;

impl ComponentManagerApi for ComponentManager {
    type ComponentManagerConfiguration = ComponentManagerConfiguration;
    type ComponentCapability = ComponentCapability;

    fn configuration(&self) -> Self::ComponentManagerConfiguration {
        todo!("TODO(fxbug.dev/111251): Integrate with production component manager API");
    }

    fn namespace_capabilities(&self) -> Box<dyn Iterator<Item = Self::ComponentCapability>> {
        todo!("TODO(fxbug.dev/111251): Integrate with production component manager API");
    }

    fn builtin_capabilities(&self) -> Box<dyn Iterator<Item = Self::ComponentCapability>> {
        todo!("TODO(fxbug.dev/111251): Integrate with production component manager API");
    }
}

pub struct System;

impl SystemApi for System {
    type DataSourcePath = PathBuf;
    type Zbi = Zbi;
    type Blob = Blob;
    type Package = ScrutinyPackage;
    type KernelFlags = KernelFlags;
    type VbMeta = VbMeta;
    type DevMgrConfiguration = DevMgrConfiguration;
    type ComponentManagerConfiguration = ComponentManagerConfiguration;

    fn build_dir(&self) -> Self::DataSourcePath {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API");
    }

    fn zbi(&self) -> Self::Zbi {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API");
    }

    fn update_package(&self) -> Self::Package {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API");
    }

    fn kernel_flags(&self) -> Self::KernelFlags {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API");
    }

    fn vb_meta(&self) -> Self::VbMeta {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API");
    }

    fn devmgr_configuration(&self) -> Self::DevMgrConfiguration {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API");
    }

    fn component_manager_configuration(&self) -> Self::ComponentManagerConfiguration {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API");
    }
}

pub struct Zbi;

impl ZbiApi for Zbi {
    type BootfsPath = PathBuf;
    type Blob = Blob;

    fn bootfs(&self) -> Box<dyn Iterator<Item = (Self::BootfsPath, Self::Blob)>> {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API");
    }
}

pub struct KernelFlags;

impl KernelFlagsApi for KernelFlags {
    fn get(&self, _key: &str) -> Option<&str> {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API");
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (String, String)>> {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API");
    }
}

pub struct VbMeta;

impl VbMetaApi for VbMeta {}

pub struct DevMgrConfiguration;

impl DevMgrConfigurationApi for DevMgrConfiguration {
    fn get(&self, _key: &str) -> Option<&str> {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API");
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (String, String)>> {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API");
    }
}

pub struct ComponentManagerConfiguration;

impl ComponentManagerConfigurationApi for ComponentManagerConfiguration {}

pub struct ComponentInstance;

impl ComponentInstanceApi for ComponentInstance {
    type Moniker = Moniker;
    type Environment = Environment;
    type Component = Component;
    type ComponentInstanceCapability = ComponentInstanceCapability;

    fn moniker(&self) -> Self::Moniker {
        todo!("TODO(fxbug.dev/111246): Integrate with production component instance API");
    }

    fn environment(&self) -> Self::Environment {
        todo!("TODO(fxbug.dev/111246): Integrate with production component instance API");
    }

    fn component(&self) -> Self::Component {
        todo!("TODO(fxbug.dev/111246): Integrate with production component instance API");
    }

    fn parent(
        &self,
    ) -> Box<
        dyn ComponentInstanceApi<
            Moniker = Self::Moniker,
            Environment = Self::Environment,
            Component = Self::Component,
            ComponentInstanceCapability = Self::ComponentInstanceCapability,
        >,
    > {
        todo!("TODO(fxbug.dev/111246): Integrate with production component instance API");
    }

    fn children(
        &self,
    ) -> Box<
        dyn Iterator<
            Item = Box<
                dyn ComponentInstanceApi<
                    Moniker = Self::Moniker,
                    Environment = Self::Environment,
                    Component = Self::Component,
                    ComponentInstanceCapability = Self::ComponentInstanceCapability,
                >,
            >,
        >,
    > {
        todo!("TODO(fxbug.dev/111246): Integrate with production component instance API");
    }

    fn descendants(
        &self,
    ) -> Box<
        dyn Iterator<
            Item = Box<
                dyn ComponentInstanceApi<
                    Moniker = Self::Moniker,
                    Environment = Self::Environment,
                    Component = Self::Component,
                    ComponentInstanceCapability = Self::ComponentInstanceCapability,
                >,
            >,
        >,
    > {
        todo!("TODO(fxbug.dev/111246): Integrate with production component instance API");
    }

    fn ancestors(
        &self,
    ) -> Box<
        dyn Iterator<
            Item = Box<
                dyn ComponentInstanceApi<
                    Moniker = Self::Moniker,
                    Environment = Self::Environment,
                    Component = Self::Component,
                    ComponentInstanceCapability = Self::ComponentInstanceCapability,
                >,
            >,
        >,
    > {
        todo!("TODO(fxbug.dev/111246): Integrate with production component instance API");
    }

    fn uses(&self) -> Box<dyn Iterator<Item = Self::ComponentInstanceCapability>> {
        todo!("TODO(fxbug.dev/111246): Integrate with production component instance API");
    }

    fn exposes(&self) -> Box<dyn Iterator<Item = Self::ComponentInstanceCapability>> {
        todo!("TODO(fxbug.dev/111246): Integrate with production component instance API");
    }

    fn offers(&self) -> Box<dyn Iterator<Item = Self::ComponentInstanceCapability>> {
        todo!("TODO(fxbug.dev/111246): Integrate with production component instance API");
    }

    fn capabilities(&self) -> Box<dyn Iterator<Item = Self::ComponentInstanceCapability>> {
        todo!("TODO(fxbug.dev/111246): Integrate with production component instance API");
    }
}

pub struct Moniker;

impl MonikerApi for Moniker {}

pub struct Environment;

impl EnvironmentApi for Environment {}

pub struct ComponentResolver;

impl ComponentResolverApi for ComponentResolver {
    type Hash = Hash;

    fn resolve(&self, _url: ComponentResolverUrl) -> Option<Self::Hash> {
        todo!("TODO(fxbug.dev/111250): Integrate with production component resolver API");
    }

    fn aliases(&self, _hash: Self::Hash) -> Box<dyn Iterator<Item = ComponentResolverUrl>> {
        todo!("TODO(fxbug.dev/111250): Integrate with production component resolver API");
    }
}

pub struct PackageResolver;

impl PackageResolverApi for PackageResolver {
    type Hash = Hash;

    fn resolve(&self, _url: PackageResolverUrl) -> Option<Self::Hash> {
        todo!("TODO(fxbug.dev/111249): Integrate with production package resolver API");
    }

    fn aliases(&self, _hash: Self::Hash) -> Box<dyn Iterator<Item = PackageResolverUrl>> {
        todo!("TODO(fxbug.dev/111249): Integrate with production package resolver API");
    }
}

pub struct Component;

impl ComponentApi for Component {
    type Package = ScrutinyPackage;
    type ComponentCapability = ComponentCapability;
    type ComponentInstance = ComponentInstance;

    fn packages(&self) -> Box<dyn Iterator<Item = Self::Package>> {
        todo!("TODO(fxbug.dev/111243): Integrate with production component API");
    }

    fn children(&self) -> Box<dyn Iterator<Item = PackageResolverUrl>> {
        todo!("TODO(fxbug.dev/111243): Integrate with production component API");
    }

    fn uses(&self) -> Box<dyn Iterator<Item = Self::ComponentCapability>> {
        todo!("TODO(fxbug.dev/111243): Integrate with production component API");
    }

    fn exposes(&self) -> Box<dyn Iterator<Item = Self::ComponentCapability>> {
        todo!("TODO(fxbug.dev/111243): Integrate with production component API");
    }

    fn offers(&self) -> Box<dyn Iterator<Item = Self::ComponentCapability>> {
        todo!("TODO(fxbug.dev/111243): Integrate with production component API");
    }

    fn capabilities(&self) -> Box<dyn Iterator<Item = Self::ComponentCapability>> {
        todo!("TODO(fxbug.dev/111243): Integrate with production component API");
    }

    fn instances(&self) -> Box<dyn Iterator<Item = Self::ComponentInstance>> {
        todo!("TODO(fxbug.dev/111243): Integrate with production component API");
    }
}

pub struct ComponentCapability;

impl ComponentCapabilityApi for ComponentCapability {
    type Component = Component;
    type CapabilityName = ComponentCapabilityName;
    type CapabilityPath = ComponentCapabilityPath;

    fn component(&self) -> Self::Component {
        todo!("TODO(fxbug.dev/111244): Integrate with production component capability API");
    }

    fn kind(&self) -> CapabilityKind {
        todo!("TODO(fxbug.dev/111244): Integrate with production component capability API");
    }

    fn source(&self) -> CapabilitySource {
        todo!("TODO(fxbug.dev/111244): Integrate with production component capability API");
    }

    fn destination(&self) -> CapabilityDestination {
        todo!("TODO(fxbug.dev/111244): Integrate with production component capability API");
    }

    fn source_name(&self) -> Option<Self::CapabilityName> {
        todo!("TODO(fxbug.dev/111244): Integrate with production component capability API");
    }

    fn destination_name(&self) -> Option<Self::CapabilityName> {
        todo!("TODO(fxbug.dev/111244): Integrate with production component capability API");
    }

    fn source_path(&self) -> Option<Self::CapabilityPath> {
        todo!("TODO(fxbug.dev/111244): Integrate with production component capability API");
    }

    fn destination_path(&self) -> Option<Self::CapabilityPath> {
        todo!("TODO(fxbug.dev/111244): Integrate with production component capability API");
    }
}

pub struct ComponentCapabilityName;

impl ComponentCapabilityNameApi for ComponentCapabilityName {
    type ComponentCapability = ComponentCapability;

    fn component(&self) -> Self::ComponentCapability {
        todo!("TODO(fxbug.dev/111244): Integrate with production component capability name API");
    }
}

pub struct ComponentCapabilityPath;

impl ComponentCapabilityPathApi for ComponentCapabilityPath {
    type ComponentCapability = ComponentCapability;

    fn component(&self) -> Self::ComponentCapability {
        todo!("TODO(fxbug.dev/111244): Integrate with production component capability path API");
    }
}
