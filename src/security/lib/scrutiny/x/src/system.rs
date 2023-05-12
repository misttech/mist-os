// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api;
use crate::todo;
use std::marker::PhantomData;
use std::path::PathBuf;

pub struct System<Blob: api::Blob, Package: api::Package> {
    // TODO(fxbug.dev/111251): Add read interfaces for system artifacts such as Zbi, kernel flags,
    // etc.
    _marker: PhantomData<(Blob, Package)>,
}

impl<Blob: api::Blob, Package: api::Package> System<Blob, Package> {
    pub(crate) fn new() -> Self {
        Self { _marker: PhantomData }
    }
}

impl<Blob: api::Blob, Package: api::Package> api::System for System<Blob, Package> {
    type DataSourcePath = PathBuf;
    type Blob = Blob;
    type Package = Package;

    // TODO(fxbug.dev/111251): Replace references to fake or todo implementations with production
    // implementations.
    type Zbi = todo::Zbi;
    type KernelFlags = todo::KernelFlags;
    type VbMeta = todo::VbMeta;
    type AdditionalBootConfiguration = todo::AdditionalBootConfiguration;
    type ComponentManagerConfiguration = todo::ComponentManagerConfiguration;

    fn build_dir(&self) -> Self::DataSourcePath {
        todo!("TODO(fxbug.dev/111251): Implement build_dir()");
    }

    fn zbi(&self) -> Self::Zbi {
        todo!("TODO(fxbug.dev/111251): Implement zbi()");
    }

    fn update_package(&self) -> Self::Package {
        todo!("TODO(fxbug.dev/111251): Implement update_package()");
    }

    fn kernel_flags(&self) -> Self::KernelFlags {
        todo!("TODO(fxbug.dev/111251): Implement kernel_flags()");
    }

    fn vb_meta(&self) -> Self::VbMeta {
        todo!("TODO(fxbug.dev/111251): Implement vb_meta()");
    }

    fn additional_boot_configuration(&self) -> Self::AdditionalBootConfiguration {
        todo!("TODO(fxbug.dev/111251): Implement additional_boot_configuration()");
    }

    fn component_manager_configuration(&self) -> Self::ComponentManagerConfiguration {
        todo!("TODO(fxbug.dev/111251): Implement component_manager_configuration()");
    }
}

#[cfg(test)]
pub mod fake {
    use crate::api;
    use crate::blob::fake::Blob as FakeBlob;
    use crate::hash::fake::Hash;
    use crate::package::fake::Package as FakePackage;
    use std::convert;
    use std::iter;
    use std::marker::PhantomData;

    #[derive(Default)]
    pub(crate) struct System<
        Blob: api::Blob = FakeBlob<Hash>,
        Package: Default + api::Package = FakePackage,
    >(PhantomData<(Blob, Package)>);

    impl<Blob: api::Blob, Package: Default + api::Package> api::System for System<Blob, Package> {
        type DataSourcePath = &'static str;
        type Zbi = Zbi;
        type Blob = Blob;
        type Package = Package;
        type KernelFlags = KernelFlags;
        type VbMeta = VbMeta;
        type AdditionalBootConfiguration = AdditionalBootConfiguration;
        type ComponentManagerConfiguration = ComponentManagerConfiguration;

        fn build_dir(&self) -> Self::DataSourcePath {
            ""
        }

        fn zbi(&self) -> Self::Zbi {
            Zbi::default()
        }

        fn update_package(&self) -> Self::Package {
            Package::default()
        }

        fn kernel_flags(&self) -> Self::KernelFlags {
            KernelFlags::default()
        }

        fn vb_meta(&self) -> Self::VbMeta {
            VbMeta::default()
        }

        fn additional_boot_configuration(&self) -> Self::AdditionalBootConfiguration {
            AdditionalBootConfiguration::default()
        }

        fn component_manager_configuration(&self) -> Self::ComponentManagerConfiguration {
            ComponentManagerConfiguration::default()
        }
    }

    /// TODO(fxbug.dev/111251): Implement for production System API.
    #[derive(Default)]
    pub(crate) struct Zbi;

    impl api::Zbi for Zbi {
        type BootfsPath = &'static str;
        type Blob = FakeBlob<Hash>;
        type Error = convert::Infallible;

        fn bootfs(
            &self,
        ) -> Result<Box<dyn Iterator<Item = (Self::BootfsPath, Self::Blob)>>, Self::Error> {
            Ok(Box::new(iter::empty()))
        }
    }

    /// TODO(fxbug.dev/111251): Implement for production System API.
    #[derive(Default)]
    pub(crate) struct KernelFlags;

    impl api::KernelFlags for KernelFlags {
        fn get(&self, _key: &str) -> Option<&str> {
            None
        }

        fn iter(&self) -> Box<dyn Iterator<Item = (String, String)>> {
            Box::new(iter::empty())
        }
    }

    /// TODO(fxbug.dev/111251): Implement for production System API.
    #[derive(Default)]
    pub(crate) struct VbMeta;

    impl api::VbMeta for VbMeta {}

    /// TODO(fxbug.dev/111251): Implement for production System API.
    #[derive(Default)]
    pub(crate) struct AdditionalBootConfiguration;

    impl api::AdditionalBootConfiguration for AdditionalBootConfiguration {
        fn get(&self, _key: &str) -> Option<&str> {
            None
        }

        fn iter(&self) -> Box<dyn Iterator<Item = (String, String)>> {
            Box::new(iter::empty())
        }
    }

    /// TODO(fxbug.dev/111251): Implement for production System API.
    #[derive(Default)]
    pub(crate) struct ComponentManagerConfiguration;

    impl api::ComponentManagerConfiguration for ComponentManagerConfiguration {}
}
