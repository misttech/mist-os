// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod collection;
mod controller;
pub mod package;
pub mod util;

use crate::core::controller::blob::*;
use crate::core::controller::component::*;
use crate::core::controller::package::*;
use crate::core::controller::package_extract::*;
use crate::core::package::collector::*;
use scrutiny::prelude::*;
use std::sync::Arc;

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
