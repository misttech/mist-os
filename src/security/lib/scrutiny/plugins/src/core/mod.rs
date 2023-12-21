// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod collection;
mod controller;
pub mod package;
pub mod util;

use {
    crate::core::{
        controller::{blob::*, component::*, package::*, package_extract::*},
        package::collector::*,
    },
    scrutiny::prelude::*,
    std::sync::Arc,
};

plugin!(
    CorePlugin,
    PluginHooks::new(
        collectors! {
            "PackageDataCollector" => PackageDataCollector::default(),
        },
        controllers! {
            "/component" => ComponentGraphController::default(),
            "/components" => ComponentsGraphController::default(),
            "/components/urls" => ComponentsUrlListController::default(),
            "/component/manifest" => ComponentManifestGraphController::default(),
            "/package/extract" => PackageExtractController::default(),
            "/packages" => PackagesGraphController::default(),
            "/packages/urls" => PackageUrlListController::default(),
            "/blob" => BlobController::default(),
        }
    ),
    vec![PluginDescriptor::new("StaticPkgsPlugin"), PluginDescriptor::new("ZbiPlugin")]
);
